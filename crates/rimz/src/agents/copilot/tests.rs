use super::*;

use std::collections::BTreeSet;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::{AgentErr, AgentHookClass, AgentStatus, LaunchPreset, PresetErr};
use serde_json::json;

#[test]
fn account_usage_surface_is_unsupported() {
    assert_eq!(
        CopilotAdapter.probe_account_usage(),
        crate::agents::AccountUsageProbe::Unsupported
    );
    assert_eq!(CopilotAdapter.account_usage_identity(), None);
}

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
            native_key: None,
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
            native_key: None,
        })
    );
    assert_eq!(
        CopilotAdapter
            .observe_lifecycle("preToolUse", &json!({"sessionId":"s","toolName":"bash"}),)
            .map(|observation| observation.signal),
        Some(LifecycleSignal::ToolUsed {
            mutates: false,
            edits: false,
            native_key: None,
        })
    );
}

#[test]
fn lifecycle_signals_drive_the_shared_state_machine() {
    let registered = observation("sessionStart", json!({"sessionId":"s","cwd":"/tmp/work"}));
    assert_eq!(registered.signal, LifecycleSignal::Registered);
    assert_eq!(registered.worktree_path.as_deref(), Some("/tmp/work"));
    assert_eq!(registered.origin, None);
    let mut state = step(None, None, &registered.signal).next;

    let started = observation(
        "userPromptSubmitted",
        json!({"sessionId":"s","prompt":"  fix auth  "}),
    );
    assert_eq!(started.task.as_deref(), Some("fix auth"));
    state = step(Some(&state), None, &started.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Reasoning);

    let tool = observation("postToolUse", json!({"sessionId":"s","toolName":"edit"}));
    state = step(Some(&state), None, &tool.signal).next;
    assert_eq!(state.phase, TurnPhase::Acting);

    let compacting = observation("preCompact", json!({"sessionId":"s","trigger":"auto"}));
    state = step(Some(&state), None, &compacting.signal).next;
    assert!(state.compacting);
    state = step(Some(&state), None, &LifecycleSignal::TurnStarted).next;
    assert!(
        !state.compacting,
        "the next lifecycle edge closes the bracket"
    );

    let stopped = observation("agentStop", json!({"sessionId":"s"}));
    state = step(Some(&state), None, &stopped.signal).next;
    assert_eq!(state.status, AgentStatus::Success);

    let ended = observation("sessionEnd", json!({"sessionId":"s"}));
    state = step(Some(&state), None, &ended.signal).next;
    assert_eq!(state.status, AgentStatus::Success);
}

#[test]
fn native_prompt_before_registration_keeps_the_turn_running() {
    let started = observation(
        "userPromptSubmitted",
        json!({"sessionId":"s","prompt":"start first"}),
    );
    let mut state = step(None, None, &started.signal).next;
    assert_eq!(state.status, AgentStatus::Running);

    let session_start = observation(
        "sessionStart",
        json!({"sessionId":"s","source":"startup","initialPrompt":"start first"}),
    );
    assert_eq!(session_start.signal, LifecycleSignal::TurnStarted);
    assert_eq!(session_start.origin, Some(SessionOrigin::Fresh));
    assert!(
        session_start.prompt.is_none(),
        "the duplicate signal carries no prompt"
    );
    state = step(Some(&state), None, &session_start.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Reasoning);

    state = step(
        Some(&state),
        None,
        &observation("agentStop", json!({"sessionId":"s"})).signal,
    )
    .next;
    assert_eq!(state.status, AgentStatus::Success);
}

#[test]
fn promptless_session_start_registers_idle() {
    for payload in [
        json!({"sessionId":"s","source":"startup"}),
        json!({"sessionId":"s","source":"startup","initialPrompt":"  "}),
    ] {
        let session_start = observation("sessionStart", payload);
        assert_eq!(session_start.signal, LifecycleSignal::Registered);
        let running = step(None, None, &LifecycleSignal::TurnStarted).next;
        assert_eq!(
            step(Some(&running), None, &session_start.signal)
                .next
                .status,
            AgentStatus::Idle,
        );
    }
}

#[test]
fn agent_stop_accepts_a_matching_native_transcript_and_reads_final_text() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("session-1");
    std::fs::create_dir(&session_dir).unwrap();
    let path = session_dir.join("events.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"assistant.message\",\"timestamp\":\"2026-07-13T15:13:23Z\",\"data\":{\"content\":\"final text\"}}\n",
    )
    .unwrap();
    let payload = json!({
        "sessionId": "session-1",
        "transcriptPath": path,
    });
    let stopped = observation("agentStop", payload.clone());
    assert_eq!(
        stopped.transcript_path.as_deref(),
        path.to_str(),
        "validated native path replaces derivation"
    );
    assert_eq!(
        CopilotAdapter
            .last_assistant_message("agentStop", &payload, &stopped)
            .as_deref(),
        Some("final text")
    );

    let mismatched = observation(
        "agentStop",
        json!({"sessionId":"other-session","transcriptPath":path}),
    );
    assert_ne!(mismatched.transcript_path.as_deref(), path.to_str());
}

#[test]
fn child_hook_without_a_transcript_does_not_publish_a_phantom_path() {
    let stopped = observation(
        "agentStop",
        json!({
            "sessionId":"toolu_child-with-no-session-state-rimz-test",
            "transcriptPath":""
        }),
    );

    assert_eq!(stopped.transcript_path, None);
}

#[test]
fn adapter_correlates_child_hook_ids_through_the_parent_transcript() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join("parent-session");
    std::fs::create_dir(&session_dir).unwrap();
    let path = session_dir.join("events.jsonl");
    std::fs::write(&path, include_str!("tests/fixtures/subagents.jsonl")).unwrap();
    let child_id = AgentSessionId::from("toolu_alpha");
    let parent_id = AgentSessionId::from("parent-session");

    assert_eq!(
        CopilotAdapter.correlate_subagent(SubagentCorrelationInput {
            child_agent_id: &child_id,
            child_workspace: Some(dir.path()),
            parent_agent_id: &parent_id,
            parent_workspace: Some(dir.path()),
            parent_transcript_path: Some(&path),
        }),
        Some(SubagentCorrelation {
            agent_name: Some("researcher".to_owned()),
            role: None,
            task: Some("Inspect auth retry".to_owned()),
            prompt: Some("Trace the retry flow".to_owned()),
        })
    );
}

#[test]
fn session_start_marks_only_fresh_identity_sources() {
    for (source, expected) in [
        ("startup", Some(SessionOrigin::Fresh)),
        ("new", Some(SessionOrigin::Fresh)),
        ("resume", None),
    ] {
        let registered = observation(
            "sessionStart",
            json!({"sessionId":"s","cwd":"/tmp/work","source":source}),
        );
        assert_eq!(registered.origin, expected, "{source}");
    }
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
                &json!({"sessionId":"s","toolName":tool,"error":"tool failed"}),
            )
            .map(|observation| observation.signal);
        let expected = expected.map(|(mutates, edits)| LifecycleSignal::ToolUsed {
            mutates,
            edits,
            native_key: None,
        });
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
            &json!({"toolName":"ask_user","toolArgs":"{\"question\":\"Which branch?\"}"}),
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
fn room_env_uses_private_defaults_and_preserves_user_exporters() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = crate::store::RuntimePaths::under(
        crate::ids::WorkspaceId::from_project_root(temp.path()),
        temp.path(),
    )
    .unwrap();
    assert_eq!(
        super::room_env_from(&runtime, None, None, None, None),
        std::collections::BTreeMap::from([
            (
                "COPILOT_OTEL_FILE_EXPORTER_PATH".to_owned(),
                runtime.copilot_otel_path().to_string_lossy().into_owned(),
            ),
            (
                "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT".to_owned(),
                "false".to_owned(),
            ),
        ])
    );
    assert_eq!(
        super::room_env_from(
            &runtime,
            Some(std::ffi::OsStr::new("/user/otel.jsonl")),
            None,
            None,
            Some(std::ffi::OsStr::new("true")),
        ),
        std::collections::BTreeMap::from([
            (
                "COPILOT_OTEL_FILE_EXPORTER_PATH".to_owned(),
                "/user/otel.jsonl".to_owned(),
            ),
            (
                "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT".to_owned(),
                "true".to_owned(),
            ),
        ])
    );
    assert_eq!(
        super::room_env_from(
            &runtime,
            None,
            Some(std::ffi::OsStr::new("http://otel")),
            None,
            None,
        ),
        std::collections::BTreeMap::from([(
            "OTEL_EXPORTER_OTLP_ENDPOINT".to_owned(),
            "http://otel".to_owned(),
        )])
    );
    assert_eq!(
        super::room_env_from(
            &runtime,
            None,
            None,
            Some(std::ffi::OsStr::new("otlp-http")),
            None,
        ),
        std::collections::BTreeMap::from([(
            "COPILOT_OTEL_EXPORTER_TYPE".to_owned(),
            "otlp-http".to_owned(),
        )])
    );
    assert_eq!(
        super::room_env_from(
            &runtime,
            None,
            Some(std::ffi::OsStr::new("http://unused")),
            Some(std::ffi::OsStr::new("file")),
            None,
        )
        .get("COPILOT_OTEL_FILE_EXPORTER_PATH"),
        Some(&runtime.copilot_otel_path().to_string_lossy().into_owned())
    );
}

#[test]
fn launch_resume_permissions_and_presets_are_pinned() {
    assert_eq!(
        CopilotAdapter.launch_command(&["--banner".to_owned()], Some("review this")),
        Some(vec![
            "copilot".to_owned(),
            "--banner".to_owned(),
            "--interactive".to_owned(),
            "review this".to_owned()
        ])
    );
    assert_eq!(
        CopilotAdapter.launch_command(&[], None),
        Some(vec!["copilot".to_owned()])
    );
    assert_eq!(CopilotAdapter.ping_args(), None);
    assert_eq!(
        CopilotAdapter.launch_env(),
        vec![(
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT",
            "false"
        )]
    );
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
    assert!(!report.files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);
    assert!(COPILOT_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "{\"_rimz_managed\":\"drifted\"}\n").unwrap();
    assert!(COPILOT_MANAGED_SOURCE.install_into(&path).unwrap().files[0].existed);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), HOOK_SOURCE);
    assert_eq!(
        COPILOT_MANAGED_SOURCE.preview_at(&path).unwrap().files[0].candidate,
        HOOK_SOURCE
    );
    assert!(COPILOT_MANAGED_SOURCE.uninstall_from(&path).unwrap().files[0].existed);
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
fn two_file_install_wraps_idempotently_and_restores_the_exact_json_value() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks/rimz.json");
    let settings = dir.path().join("settings.json");
    let original_statusline = json!({
        "type": "command",
        "command": "printf user-status",
        "padding": 0
    });
    std::fs::write(
        &settings,
        serde_json::to_string_pretty(&json!({
            "theme": "dark",
            "statusLine": original_statusline,
        }))
        .unwrap(),
    )
    .unwrap();

    let preview = install::preview(&hooks, &settings).unwrap();
    assert_eq!(preview.files.len(), 2);
    assert_eq!(
        preview.status_line_change,
        Some(crate::agents::StatusLineChange::Wrapping {
            original: "printf user-status".to_owned(),
        })
    );
    assert_eq!(preview.planned_events.len(), WIRED_EVENTS.len());

    install::install(&hooks, &settings).unwrap();
    assert!(install::installed(&hooks, &settings));
    assert!(install::managed(&hooks, &settings));
    assert_eq!(
        install::wrapped_statusline_command(&settings).as_deref(),
        Some("printf user-status")
    );
    let installed: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(installed["theme"], "dark");
    assert_eq!(installed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert_eq!(installed["statusLine"]["padding"], 0);
    assert_eq!(
        installed["statusLine"]["_rimz_wrapped"],
        original_statusline
    );

    let once = std::fs::read(&settings).unwrap();
    install::install(&hooks, &settings).unwrap();
    assert_eq!(std::fs::read(&settings).unwrap(), once);
    let reinstalled: Value = serde_json::from_slice(&once).unwrap();
    assert_eq!(
        reinstalled["statusLine"]["_rimz_wrapped"], original_statusline,
        "reinstall must not nest the managed wrapper"
    );

    install::uninstall(&hooks, &settings).unwrap();
    assert!(!hooks.exists());
    let restored: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored["theme"], "dark");
    assert_eq!(restored["statusLine"], original_statusline);
}

#[test]
fn partial_installs_are_detected_and_cleaned_independently() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks/rimz.json");
    let settings = dir.path().join("settings.json");

    COPILOT_MANAGED_SOURCE.install_into(&hooks).unwrap();
    std::fs::write(&settings, "{\"theme\":\"user\"}\n").unwrap();
    assert!(!install::installed(&hooks, &settings));
    assert!(install::managed(&hooks, &settings));
    install::uninstall(&hooks, &settings).unwrap();
    assert!(!hooks.exists());
    assert_eq!(
        std::fs::read_to_string(&settings).unwrap(),
        "{\"theme\":\"user\"}\n"
    );

    install::install(&hooks, &settings).unwrap();
    std::fs::remove_file(&hooks).unwrap();
    assert!(!install::installed(&hooks, &settings));
    assert!(install::managed(&hooks, &settings));
    install::uninstall(&hooks, &settings).unwrap();
    let restored: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(restored, json!({"theme":"user"}));
}

#[test]
fn install_refuses_conflicts_and_strict_json_without_touching_files() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("rimz.json");
    let settings = dir.path().join("settings.json");
    std::fs::write(&hooks, "{\"version\":1}\n").unwrap();
    let error = install::preview(&hooks, &settings).unwrap_err();
    assert!(error.to_string().contains("unmarked user hook file"));
    assert!(!settings.exists());

    std::fs::remove_file(&hooks).unwrap();
    for text in [
        "{\"statusLine\":\"plain string\"}\n",
        "{\"statusLine\":{\"type\":\"preset\",\"command\":\"user\"}}\n",
        "{\"statusLine\":{\"type\":\"command\"}}\n",
        "{\n // comment\n \"statusLine\": null\n}\n",
        "{\"statusLine\": null,}\n",
    ] {
        std::fs::write(&settings, text).unwrap();
        assert!(install::install(&hooks, &settings).is_err(), "{text}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), text);
        assert!(!hooks.exists());
    }
}

#[test]
fn marker_recovery_does_not_wrap_rimz_recursively() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks/rimz.json");
    let settings = dir.path().join("settings.json");
    std::fs::write(
        &settings,
        serde_json::to_vec(&json!({
            "statusLine": {
                "type": "command",
                "command": format!("env X=1 {RIMZ_STATUS_LINE_MARKER}"),
                "padding": 2
            }
        }))
        .unwrap(),
    )
    .unwrap();

    install::install(&hooks, &settings).unwrap();

    let installed: Value = serde_json::from_slice(&std::fs::read(&settings).unwrap()).unwrap();
    assert_eq!(installed["statusLine"]["command"], STATUS_LINE_COMMAND);
    assert_eq!(installed["statusLine"]["padding"], 2);
    assert!(installed["statusLine"].get("_rimz_wrapped").is_none());
    assert_eq!(install::wrapped_statusline_command(&settings), None);
}

#[test]
fn read_only_statusline_probes_accept_later_jsonc_edits() {
    let dir = tempfile::tempdir().unwrap();
    let hooks = dir.path().join("hooks/rimz.json");
    let settings = dir.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{"statusLine":{"type":"command","command":"printf user"}}"#,
    )
    .unwrap();
    install::install(&hooks, &settings).unwrap();
    let mut installed = std::fs::read_to_string(&settings).unwrap();
    installed.insert_str(2, "// retained user comment\n");
    installed = installed.replacen("\n}", ",\n}\n", 1);
    std::fs::write(&settings, installed).unwrap();

    assert!(install::statusline_installed(&settings));
    assert_eq!(
        install::wrapped_statusline_command(&settings).as_deref(),
        Some("printf user")
    );
    assert!(install::uninstall(&hooks, &settings).is_err());
    assert!(
        hooks.exists(),
        "strict uninstall fails before removing hooks"
    );
}

#[test]
fn two_file_transactions_roll_back_both_install_and_uninstall_failures() {
    let dir = tempfile::tempdir().unwrap();
    let blocked_parent = dir.path().join("blocked");
    let hooks = blocked_parent.join("rimz.json");
    let settings = dir.path().join("settings.json");
    std::fs::write(&blocked_parent, "not a directory").unwrap();
    assert!(install::install(&hooks, &settings).is_err());
    assert!(
        !settings.exists(),
        "failed hook write restores absent settings"
    );

    std::fs::remove_file(&blocked_parent).unwrap();
    install::install(&hooks, &settings).unwrap();
    let installed_settings = std::fs::read(&settings).unwrap();
    let error = install::uninstall_with(&hooks, &settings, |_| {
        Err(AgentErr::Install {
            agent: "copilot",
            reason: "injected hook removal failure".to_owned(),
        })
    })
    .unwrap_err();
    assert!(error.to_string().contains("injected hook removal failure"));
    assert_eq!(std::fs::read(&settings).unwrap(), installed_settings);
    assert!(hooks.exists());
}

#[test]
fn statusline_health_suppresses_otel_and_replacement_restores_it() {
    let dir = tempfile::tempdir().unwrap();
    let otel = dir.path().join("otel.jsonl");
    std::fs::write(&otel, include_str!("tests/fixtures/otel.jsonl")).unwrap();
    let pricing = dir.path().join("pricing.json");
    let ctx = LocalContextRefreshCtx {
        agent_id: "session-fixture",
        model_hint: None,
        current_transcript_path: None,
        prior_transcript_path: otel.to_str(),
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    };

    assert!(
        local_context_refresh_with_statusline(true, RefreshTrigger::Tick, &ctx).is_none(),
        "a healthy statusline is the authoritative enrichment source"
    );
    let fallback = local_context_refresh_with_statusline(false, RefreshTrigger::Tick, &ctx)
        .expect("missing or replaced statusline restores OTel");
    assert_eq!(fallback.model_id.as_deref(), Some("gpt-5-mini"));
    assert!(fallback.tokens.is_some());
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
