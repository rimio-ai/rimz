use super::*;

use std::collections::BTreeSet;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::{AgentErr, AgentHookClass, AgentStatus, LaunchPreset, PresetErr};
use serde_json::json;

#[test]
fn classifies_native_asks_and_lifecycle_events() {
    let permission = CopilotAdapter.classify_hook(
        "permissionRequest",
        &json!({"sessionId":"s","toolName":"bash"}),
    );
    assert_eq!(permission.class, AgentHookClass::AwaitingUser);
    assert_eq!(permission.ask_kind, Some(AskKind::Permission));

    let question = CopilotAdapter.classify_hook(
        "preToolUse",
        &json!({"sessionId":"s","toolName":"ask_user"}),
    );
    assert_eq!(question.class, AgentHookClass::AwaitingUser);
    assert_eq!(question.ask_kind, Some(AskKind::Question));

    assert_eq!(
        CopilotAdapter
            .classify_hook("preToolUse", &json!({"sessionId":"s","toolName":"bash"}),)
            .class,
        AgentHookClass::Lifecycle
    );
    assert_eq!(
        CopilotAdapter
            .observe_lifecycle(
                "permissionRequest",
                &json!({"sessionId":"s","toolName":"bash"}),
            )
            .map(|observation| observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Permission,
            ask_id: None,
            detail: None,
        })
    );
    assert_eq!(
        CopilotAdapter
            .observe_lifecycle(
                "preToolUse",
                &json!({"sessionId":"s","toolName":"ask_user"}),
            )
            .map(|observation| observation.signal),
        Some(LifecycleSignal::AwaitingInput {
            kind: AskKind::Question,
            ask_id: None,
            detail: None,
        })
    );
    assert_eq!(
        CopilotAdapter
            .observe_lifecycle("preToolUse", &json!({"sessionId":"s","toolName":"bash"}),)
            .map(|observation| observation.signal),
        Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
        })
    );
}

#[test]
fn lifecycle_signals_drive_the_shared_state_machine() {
    let registered = observation("sessionStart", json!({"sessionId":"s","cwd":"/tmp/work"}));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.worktree_path.as_deref(), Some("/tmp/work"));
    let mut state = step(None, &registered.signal).next;

    let started = observation(
        "userPromptSubmitted",
        json!({"sessionId":"s","prompt":"  fix auth  "}),
    );
    assert_eq!(started.task.as_deref(), Some("fix auth"));
    state = step(Some(&state), &started.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Reasoning);

    let tool = observation("postToolUse", json!({"sessionId":"s","toolName":"edit"}));
    state = step(Some(&state), &tool.signal).next;
    assert_eq!(state.phase, TurnPhase::Acting);

    let compacting = observation("preCompact", json!({"sessionId":"s","trigger":"auto"}));
    state = step(Some(&state), &compacting.signal).next;
    assert!(state.compacting);
    state = step(Some(&state), &LifecycleSignal::TurnStarted).next;
    assert!(
        !state.compacting,
        "the next lifecycle edge closes the bracket"
    );

    let stopped = observation("agentStop", json!({"sessionId":"s"}));
    state = step(Some(&state), &stopped.signal).next;
    assert_eq!(state.status, AgentStatus::Success);

    let ended = observation("sessionEnd", json!({"sessionId":"s"}));
    state = step(Some(&state), &ended.signal).next;
    assert_eq!(state.status, AgentStatus::Success);
}

#[test]
fn tool_mapping_uses_camel_case_names() {
    for (tool, expected) in [
        ("edit", Some((true, true))),
        ("bash", Some((true, false))),
        ("read", None),
    ] {
        let signal = CopilotAdapter
            .observe_lifecycle(
                "postToolUseFailure",
                &json!({"sessionId":"s","toolName":tool}),
            )
            .map(|observation| observation.signal);
        let expected =
            expected.map(|(mutates, edits)| LifecycleSignal::ToolUsed { mutates, edits });
        assert_eq!(signal, expected, "{tool}");
    }
}

#[test]
fn error_marker_only_accepts_non_recoverable_errors() {
    let marker = CopilotAdapter
        .observe_turn_error_from_hook(
            "errorOccurred",
            &json!({
                "sessionId":"s",
                "timestamp":1700000000000i64,
                "recoverable":false,
                "errorContext":"model_call",
                "error":{"name":"Error","message":"network error"}
            }),
        )
        .expect("marker");
    assert_eq!(marker.class, TurnErrorClass::PausedOverloaded);
    assert_eq!(marker.label.as_deref(), Some("network error"));
    assert_eq!(marker.at, Timestamp::from_second(1_700_000_000).unwrap());
    assert!(
        CopilotAdapter
            .observe_turn_error_from_hook(
                "errorOccurred",
                &json!({"recoverable":true,"error":{"message":"retry"}}),
            )
            .is_none()
    );
    assert!(
        CopilotAdapter
            .observe_turn_error_from_hook(
                "errorOccurred",
                &json!({"recoverable":false,"error":{}}),
            )
            .is_none()
    );
}

#[test]
fn ask_details_are_best_effort() {
    let questions = CopilotAdapter
        .ask_question_detail(
            "preToolUse",
            &json!({"toolName":"ask_user","toolArgs":{"question":"Which branch?"}}),
        )
        .expect("question");
    assert_eq!(questions[0].question, "Which branch?");
    assert_eq!(
        CopilotAdapter.ask_detail("permissionRequest", &json!({"toolName":"powershell"}),),
        Some("powershell".to_owned())
    );
    assert!(
        CopilotAdapter
            .ask_question_detail("preToolUse", &json!({"toolName":"ask_user","toolArgs":{}}))
            .is_none()
    );
}

#[test]
fn neutral_output_is_empty_for_both_blocking_events() {
    let question = CopilotAdapter.render_neutral("preToolUse").unwrap();
    insta::assert_snapshot!(format!("{question:?}"), @"None");
    let permission = CopilotAdapter.render_neutral("permissionRequest").unwrap();
    insta::assert_snapshot!(format!("{permission:?}"), @"None");
}

#[test]
fn malformed_payloads_degrade_without_inventing_lifecycle_data() {
    let malformed = CopilotAdapter
        .observe_lifecycle("userPromptSubmitted", &json!(null))
        .expect("known event still maps");
    assert!(malformed.agent_id.is_none());
    assert!(malformed.prompt.is_none());
    insta::assert_json_snapshot!(json!({
        "class": format!("{:?}", CopilotAdapter.classify_hook("preToolUse", &json!(null)).class),
        "observation": format!("{:?}", malformed.signal),
    }), @r###"
    {
      "class": "Lifecycle",
      "observation": "TurnStarted"
    }
    "###);
}

#[test]
fn launch_resume_permissions_and_presets_are_pinned() {
    assert_eq!(
        CopilotAdapter.launch_command(&["--banner".to_owned()], None),
        Some(vec!["copilot".to_owned(), "--banner".to_owned()])
    );
    assert_eq!(CopilotAdapter.launch_command(&[], Some("prompt")), None);
    assert_eq!(CopilotAdapter.ping_args(), None);
    assert_eq!(
        CopilotAdapter.resume_command("sess-1", Path::new("/tmp")),
        Some(vec![
            "copilot".to_owned(),
            "--resume".to_owned(),
            "sess-1".to_owned()
        ])
    );
    assert_eq!(
        CopilotAdapter.fork_command("sess-1", Path::new("/tmp")),
        None
    );
    assert_eq!(
        CopilotAdapter.permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        CopilotAdapter.permission_args(PermissionMode::Plan),
        vec!["--plan"]
    );
    assert_eq!(
        CopilotAdapter.permission_args(PermissionMode::Auto),
        vec!["--autopilot"]
    );
    assert_eq!(
        CopilotAdapter.permission_args(PermissionMode::Yolo),
        vec!["--allow-all"]
    );
    assert_eq!(
        CopilotAdapter.render_preset(&LaunchPreset {
            model: Some("gpt-5".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "gpt-5".to_owned(),
            "--effort".to_owned(),
            "high".to_owned()
        ])
    );
    assert_eq!(
        CopilotAdapter.render_preset(&LaunchPreset {
            system_prompt_file: Some(PathBuf::from("/tmp/system.md")),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "copilot",
            field: "system-prompt-file"
        })
    );
}

#[test]
fn process_names_include_launchers_and_target_triples() {
    let descriptor = CopilotAdapter.descriptor();
    assert!(descriptor.runs_as("copilot"));
    assert!(descriptor.runs_as("node"));
    assert!(descriptor.runs_as("copilot-aarch64-unknown-linux-gnu"));
    assert!(!descriptor.runs_as("zsh"));
}

#[test]
fn install_preview_reclaim_and_uninstall_own_only_marked_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    let report = COPILOT_MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(!report.merged);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);
    assert!(COPILOT_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "{\"_rimz_managed\":\"drifted\"}\n").unwrap();
    assert!(COPILOT_MANAGED_SOURCE.install_into(&path).unwrap().merged);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);
    assert_eq!(
        COPILOT_MANAGED_SOURCE
            .preview_at(&path)
            .unwrap()
            .candidate_config,
        HOOK_SOURCE
    );
    assert!(
        COPILOT_MANAGED_SOURCE
            .uninstall_from(&path)
            .unwrap()
            .existed
    );
    assert!(!path.exists());

    let user_path = dir.path().join("user.json");
    std::fs::write(&user_path, "{\"version\":1}\n").unwrap();
    assert!(matches!(
        COPILOT_MANAGED_SOURCE.install_into(&user_path).unwrap_err(),
        AgentErr::Install {
            agent: "copilot",
            ..
        }
    ));
    assert!(
        COPILOT_MANAGED_SOURCE
            .uninstall_from(&user_path)
            .unwrap()
            .removed_events
            .is_empty()
    );
    assert!(user_path.exists());
}

#[test]
fn embedded_hook_file_matches_the_declared_wire() {
    let document: Value = serde_json::from_str(HOOK_SOURCE).expect("valid hooks JSON");
    let hooks = document["hooks"].as_object().expect("hooks object");
    let actual: BTreeSet<_> = hooks.keys().map(String::as_str).collect();
    let expected: BTreeSet<_> = WIRED_EVENTS.iter().copied().collect();
    assert_eq!(actual, expected);
    for (event, entries) in hooks {
        let entries = entries.as_array().expect("entry list");
        assert_eq!(entries.len(), 1, "{event}");
        let command = entries[0]["bash"].as_str().expect("bash command");
        assert!(
            command.contains("rimz hooks feed --source copilot"),
            "{event}"
        );
        assert!(command.ends_with(event), "{event}");
        assert_eq!(entries[0]["timeoutSec"], 30);
    }
}

fn observation(event: &str, payload: Value) -> AgentLifecycleObservation {
    CopilotAdapter
        .observe_lifecycle(event, &payload)
        .expect("observation")
}
