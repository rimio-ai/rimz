use super::*;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::{AgentHookClass, AgentStatus, LaunchPreset, PresetErr};
use serde_json::json;

#[test]
fn launch_resume_permission_and_preset_surfaces_are_native() {
    assert_eq!(
        GeminiAdapter.launch_command(&["--sandbox".to_owned()], Some("fix auth")),
        Some(vec![
            "gemini".to_owned(),
            "--sandbox".to_owned(),
            "--".to_owned(),
            "fix auth".to_owned(),
        ])
    );
    assert_eq!(
        GeminiAdapter.resume_command("12345678-abcd", Path::new("/tmp")),
        Some(vec![
            "gemini".to_owned(),
            "--resume".to_owned(),
            "12345678-abcd".to_owned(),
        ])
    );
    assert_eq!(
        GeminiAdapter.fork_command("12345678", Path::new("/tmp")),
        None
    );
    assert_eq!(GeminiAdapter.compact_command(), Some("/compress"));
    assert_eq!(
        GeminiAdapter.permission_args(PermissionMode::Ask),
        Vec::<String>::new()
    );
    assert_eq!(
        GeminiAdapter.permission_args(PermissionMode::Auto),
        vec!["--approval-mode", "auto_edit"]
    );
    assert_eq!(
        GeminiAdapter.permission_args(PermissionMode::Plan),
        vec!["--approval-mode", "plan"]
    );
    assert_eq!(
        GeminiAdapter.permission_args(PermissionMode::Yolo),
        vec!["--approval-mode", "yolo"]
    );
    assert_eq!(GeminiAdapter.ping_args(), Some(Vec::new()));
    assert_eq!(
        GeminiAdapter.render_preset(&LaunchPreset {
            model: Some("gemini-3-pro-preview".to_owned()),
            ..LaunchPreset::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "gemini-3-pro-preview".to_owned()
        ])
    );
    assert_eq!(
        GeminiAdapter.render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..LaunchPreset::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "gemini",
            field: "effort",
        })
    );
}

#[test]
fn install_preview_reinstall_drift_and_uninstall_preserve_user_hooks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
          "theme": "Default",
          "hooks": {
            "BeforeTool": [{"matcher":"read_file","hooks":[{"type":"command","name":"mine","command":"echo mine"}]}],
            "SessionStart": [{"matcher":"*","hooks":[{"type":"command","name":"stale","command":"rimz hooks feed --source gemini --old"}]}]
          }
        }"#,
    )
    .unwrap();

    assert!(
        !install::hooks_installed_at(&path),
        "under-wired config re-offers install"
    );
    let preview = install::preview_at(&path).unwrap();
    assert_eq!(preview.planned_events.len(), INSTALLED_EVENTS.len());
    assert!(preview.candidate_config.contains("echo mine"));
    assert!(preview.candidate_config.contains("RIMZ_AGENT_PID=$PPID"));
    assert!(!install::hooks_installed_at(&path));

    install::install_into(&path).unwrap();
    assert!(install::hooks_installed_at(&path));
    install::install_into(&path).unwrap();
    let installed: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for event in INSTALLED_EVENTS {
        let owned = installed["hooks"][event]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| {
                install::managed_artifacts_at(&path)
                    && entry.to_string().contains(RIMZ_HOOK_COMMAND)
            })
            .count();
        assert_eq!(owned, 1, "{event} idempotent reinstall");
    }
    assert!(installed.to_string().contains("echo mine"));

    let report = install::uninstall_from(&path).unwrap();
    assert!(!report.removed_events.is_empty());
    let uninstalled = std::fs::read_to_string(&path).unwrap();
    assert!(uninstalled.contains("echo mine"));
    assert!(!uninstalled.contains("rimz hooks feed --source gemini"));
}

#[test]
fn disabled_hook_configuration_is_not_reported_as_installed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    install::install_into(&path).unwrap();
    let mut root: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    root["hooksConfig"] = json!({"enabled": false});
    std::fs::write(&path, serde_json::to_vec_pretty(&root).unwrap()).unwrap();
    assert!(!install::hooks_installed_at(&path));
}

#[test]
fn lifecycle_mapping_steps_through_acting_compaction_and_end() {
    let registered = observe(
        "SessionStart",
        json!({
            "session_id":"sess-1", "source":"startup", "cwd":"/repo"
        }),
    );
    assert_eq!(registered.origin, Some(SessionOrigin::Fresh));
    assert_eq!(registered.worktree_path.as_deref(), Some("/repo"));
    let mut state = step(None, &registered.signal).next;
    assert_eq!(state.status, AgentStatus::Idle);

    let started = observe(
        "BeforeAgent",
        json!({
            "session_id":"sess-1", "prompt":"  fix auth  "
        }),
    );
    assert_eq!(started.prompt.as_deref(), Some("fix auth"));
    state = step(Some(&state), &started.signal).next;
    assert_eq!(state.phase, TurnPhase::Reasoning);

    assert!(
        GeminiAdapter
            .observe_lifecycle(
                "BeforeTool",
                &json!({
                    "session_id":"sess-1", "tool_name":"write_file"
                })
            )
            .is_none()
    );
    let tool = observe(
        "AfterTool",
        json!({
            "session_id":"sess-1", "tool_name":"write_file"
        }),
    );
    state = step(Some(&state), &tool.signal).next;
    assert_eq!(state.phase, TurnPhase::Acting);

    let ended_turn = observe("AfterAgent", json!({"session_id":"sess-1"}));
    state = step(Some(&state), &ended_turn.signal).next;
    assert_eq!(state.status, AgentStatus::Success);

    let compact = observe(
        "PreCompress",
        json!({"session_id":"sess-1","trigger":"manual"}),
    );
    state = step(Some(&state), &compact.signal).next;
    assert!(state.compacting);
    state = step(Some(&state), &started.signal).next;
    assert!(
        !state.compacting,
        "next lifecycle signal closes Gemini's bracket"
    );

    let ended = observe("SessionEnd", json!({"session_id":"sess-1"}));
    let before_end = state;
    state = step(Some(&state), &ended.signal).next;
    assert_eq!(state, before_end, "tombstone reducer handles session end");

    let resumed = observe(
        "SessionStart",
        json!({"session_id":"sess-2","source":"resume"}),
    );
    assert_eq!(resumed.origin, None);
    let cleared = observe(
        "SessionStart",
        json!({"session_id":"sess-3","source":"clear"}),
    );
    assert_eq!(cleared.origin, Some(SessionOrigin::Fresh));
}

#[test]
fn asks_classify_and_preserve_structured_question_detail() {
    let payload = json!({
        "session_id":"sess-1",
        "tool_name":"ask_user",
        "tool_input":{"questions":[{
            "question":"Choose a database",
            "header":"Database",
            "type":"choice",
            "multiSelect":true,
            "options":[
                {"label":"SQLite","description":"Local"},
                {"label":"Postgres","description":"Server"}
            ]
        }]}
    });
    let classified = GeminiAdapter.classify_hook("BeforeTool", &payload);
    assert_eq!(classified.class, AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind, Some(AskKind::Question));
    let detail = GeminiAdapter
        .ask_question_detail("BeforeTool", &payload)
        .unwrap();
    assert_eq!(detail[0].question, "Choose a database");
    assert!(detail[0].multi_select);
    assert_eq!(detail[0].options[0].label, "SQLite");

    let plan = GeminiAdapter.classify_hook(
        "BeforeTool",
        &json!({"tool_name":"exit_plan_mode","tool_input":{"plan_path":"/tmp/plan.md"}}),
    );
    assert_eq!(plan.ask_kind, Some(AskKind::PlanApproval));
    for (kind, expected) in [
        ("exec", AskKind::Permission),
        ("edit", AskKind::Permission),
        ("ask_user", AskKind::Question),
        ("exit_plan_mode", AskKind::PlanApproval),
        ("future", AskKind::Permission),
    ] {
        let payload = json!({"details":{"type":kind,"title":"Confirm"},"message":"Allow?"});
        let classified = GeminiAdapter.classify_hook("Notification", &payload);
        assert_eq!(classified.ask_kind, Some(expected), "{kind}");
        assert_eq!(
            GeminiAdapter
                .ask_detail("Notification", &payload)
                .as_deref(),
            Some("Allow?")
        );
    }
}

#[test]
fn neutral_output_is_gemini_empty_object_and_malformed_payloads_fail_soft() {
    insta::allow_duplicates! {
        for event in ["BeforeTool", "Notification"] {
            let neutral = GeminiAdapter.render_neutral(event).unwrap().unwrap();
            insta::assert_json_snapshot!(neutral, @r###"{}"###);
        }
    }
    assert_eq!(GeminiAdapter.render_neutral("Unknown").unwrap(), None);
    assert!(
        GeminiAdapter
            .observe_lifecycle("AfterTool", &json!("bad"))
            .is_none()
    );
    assert_eq!(
        GeminiAdapter
            .classify_hook("Notification", &json!("bad"))
            .ask_kind,
        Some(AskKind::Permission)
    );
    assert!(RIMZ_HOOK_COMMAND.starts_with("RIMZ_AGENT_PID=$PPID "));
}

#[test]
fn context_tail_maps_usage_zero_unreadable_and_model_windows() {
    let dir = tempfile::tempdir().unwrap();
    let transcript = dir.path().join("session-2026-06-02T10-00-12345678.jsonl");
    let pricing = dir.path().join("pricing.json");
    std::fs::write(
        &transcript,
        r#"{"sessionId":"12345678-abcd"}
{"id":"a","timestamp":"2026-06-02T10:00:00Z","type":"gemini","model":"gemini-3-pro-preview","tokens":{"input":120,"output":20,"cached":40,"thoughts":5,"total":100000}}"#,
    )
    .unwrap();
    let path = transcript.to_string_lossy().into_owned();
    let refresh = refresh_transcript_context(&LocalContextRefreshCtx {
        agent_id: "12345678-abcd",
        model_hint: None,
        prior_transcript_path: Some(&path),
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    })
    .unwrap();
    assert_eq!(refresh.model_id.as_deref(), Some("gemini-3-pro-preview"));
    let tokens = refresh.tokens.unwrap();
    assert_eq!(tokens.context_window_size, Some(GEMINI_CONTEXT_WINDOW));
    assert_eq!(tokens.used_percentage, Some(9));
    let usage = tokens.current_usage.unwrap();
    assert_eq!(usage.input_tokens, Some(80));
    assert_eq!(usage.cache_read_input_tokens, Some(40));
    assert_eq!(usage.output_tokens, Some(25));
    assert!(refresh.cost.and_then(|cost| cost.total_cost_usd).is_some());

    let stat = transcript_stat(&transcript).unwrap();
    assert!(
        refresh_transcript_context(&LocalContextRefreshCtx {
            agent_id: "12345678-abcd",
            model_hint: None,
            prior_transcript_path: Some(&path),
            prior_transcript_stat: Some(&stat),
            shared_pricing_cache_path: &pricing,
        })
        .is_none()
    );

    let empty = dir.path().join("session-empty-87654321.jsonl");
    std::fs::write(&empty, "{\"sessionId\":\"87654321-abcd\"}\n").unwrap();
    let empty_path = empty.to_string_lossy().into_owned();
    let empty_refresh = refresh_transcript_context(&LocalContextRefreshCtx {
        agent_id: "87654321-abcd",
        model_hint: Some("gemma-4-local"),
        prior_transcript_path: Some(&empty_path),
        prior_transcript_stat: None,
        shared_pricing_cache_path: &pricing,
    })
    .unwrap();
    let empty_tokens = empty_refresh.tokens.unwrap();
    assert_eq!(empty_tokens.used_percentage, Some(0));
    assert_eq!(empty_tokens.context_window_size, Some(GEMMA_CONTEXT_WINDOW));

    assert!(transcript_snapshot(&dir.path().join("missing.jsonl")).is_none());
}

#[test]
fn session_transcript_matches_only_the_first_eight_id_characters() {
    let files = vec![
        PathBuf::from("session-2026-06-02T10-00-deadbeef.jsonl"),
        PathBuf::from("session-2026-06-02T10-00-12345678.jsonl"),
    ];
    assert_eq!(
        find_session_transcript(files.clone(), "12345678"),
        Some(files[1].clone())
    );
    assert_eq!(find_session_transcript(files, "12345678-abcd"), None);
}

fn observe(event: &str, payload: Value) -> AgentLifecycleObservation {
    GeminiAdapter
        .observe_lifecycle(event, &payload)
        .unwrap_or_else(|| panic!("{event} observation"))
}
