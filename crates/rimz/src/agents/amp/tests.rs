use super::*;

use crate::agents::{AgentErr, AgentHookClass, AgentStatus, LaunchPreset, TurnPhase, step};
use serde_json::json;

#[test]
fn launch_resume_and_preset_commands_match_amp_cli() {
    assert_eq!(
        AmpAdapter.launch_command(&[], None),
        Some(vec!["amp".to_owned()])
    );
    assert_eq!(
        AmpAdapter.launch_command(&["--mode".to_owned(), "high".to_owned()], Some("fix auth")),
        Some(vec![
            "amp".to_owned(),
            "--mode".to_owned(),
            "high".to_owned(),
            "-x".to_owned(),
            "fix auth".to_owned(),
            "--plugin-ready-timeout".to_owned(),
            "30".to_owned(),
        ])
    );
    assert_eq!(
        AmpAdapter.resume_command("T-abc123", Path::new("/tmp")),
        Some(vec![
            "amp".to_owned(),
            "threads".to_owned(),
            "continue".to_owned(),
            "T-abc123".to_owned(),
        ])
    );
    assert_eq!(AmpAdapter.fork_command("T-abc123", Path::new("/tmp")), None);
    assert_eq!(AmpAdapter.compact_command(), None);

    assert_eq!(
        AmpAdapter.render_preset(&LaunchPreset {
            model: Some("ultra".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--mode".to_owned(),
            "ultra".to_owned(),
            "--effort".to_owned(),
            "high".to_owned(),
        ])
    );
    assert!(matches!(
        AmpAdapter.render_preset(&LaunchPreset {
            system_prompt_file: Some(PathBuf::from("/tmp/prompt")),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "amp",
            field: "system-prompt-file"
        })
    ));
}

#[test]
fn lifecycle_events_map_through_the_shared_state_machine() {
    let registered = AmpAdapter
        .observe_lifecycle(
            "session_start",
            &json!({
                "session_id": "T-abc123",
                "cwd": "/tmp/repo",
                "model": "high",
                "effort": "xhigh"
            }),
        )
        .unwrap();
    assert_eq!(registered.agent_id.as_deref(), Some("T-abc123"));
    assert_eq!(registered.worktree_path.as_deref(), Some("/tmp/repo"));
    assert_eq!(registered.launch.model.as_deref(), Some("high"));
    assert_eq!(registered.launch.effort.as_deref(), Some("xhigh"));
    assert_eq!(registered.origin, Some(SessionOrigin::Fresh));
    let mut state = step(None, &registered.signal).next;
    assert_eq!(state.status, AgentStatus::Idle);

    let started = AmpAdapter
        .observe_lifecycle(
            "agent_start",
            &json!({ "session_id": "T-abc123", "prompt": "  fix auth  " }),
        )
        .unwrap();
    assert_eq!(started.prompt.as_deref(), Some("fix auth"));
    assert_eq!(started.task.as_deref(), Some("fix auth"));
    state = step(Some(&state), &started.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Reasoning);

    let tool = AmpAdapter
        .observe_lifecycle(
            "tool_result",
            &json!({
                "session_id": "T-abc123",
                "tool_name": "unknown_dynamic_tool",
                "files_modified": true,
                "status": "done"
            }),
        )
        .unwrap();
    state = step(Some(&state), &tool.signal).next;
    assert_eq!(state.status, AgentStatus::Running);
    assert_eq!(state.phase, TurnPhase::Acting);

    let waiting = AmpAdapter
        .observe_lifecycle("permission_ask", &json!({ "session_id": "T-abc123" }))
        .unwrap();
    state = step(Some(&state), &waiting.signal).next;
    assert_eq!(state.status, AgentStatus::Waiting);

    for (status, expected) in [
        ("done", AgentStatus::Success),
        ("error", AgentStatus::Failed),
        ("cancelled", AgentStatus::Failed),
    ] {
        let ended = AmpAdapter
            .observe_lifecycle(
                "agent_end",
                &json!({ "session_id": "T-abc123", "status": status }),
            )
            .unwrap();
        assert_eq!(step(Some(&state), &ended.signal).next.status, expected);
    }
}

#[test]
fn files_modified_flag_precedes_static_tool_fallback() {
    for (payload, edits) in [
        (
            json!({ "session_id": "T-abc123", "tool_name": "read", "files_modified": true }),
            true,
        ),
        (
            json!({ "session_id": "T-abc123", "tool_name": "apply_patch", "files_modified": false }),
            false,
        ),
        (
            json!({ "session_id": "T-abc123", "tool_name": "apply_patch" }),
            true,
        ),
    ] {
        let observed = AmpAdapter
            .observe_lifecycle("tool_result", &payload)
            .unwrap();
        assert_eq!(
            observed.signal,
            LifecycleSignal::ToolUsed {
                mutates: true,
                edits,
            }
        );
    }
}

#[test]
fn agent_end_extracts_the_supervised_final_answer() {
    let payload = json!({
        "session_id": "T-abc123",
        "status": "done",
        "last_assistant_message": "  Fixed the race.  "
    });
    let observation = AmpAdapter.observe_lifecycle("agent_end", &payload).unwrap();
    assert_eq!(
        AmpAdapter
            .last_assistant_message("agent_end", &payload, &observation)
            .as_deref(),
        Some("Fixed the race.")
    );
    assert_eq!(
        AmpAdapter.last_assistant_message("tool_result", &payload, &observation),
        None
    );
}

#[test]
fn ask_classification_and_neutral_output_are_pinned() {
    let classified =
        AmpAdapter.classify_hook("permission_ask", &json!({ "session_id": "T-abc123" }));
    assert_eq!(classified.class, AgentHookClass::AwaitingUser);
    assert_eq!(classified.ask_kind, Some(AskKind::Permission));
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.render_neutral("permission_ask").unwrap()),
        @"None"
    );
}

#[test]
fn malformed_payloads_fold_to_no_observation() {
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.observe_lifecycle("agent_start", &json!({ "prompt": "missing id" }))),
        @"None"
    );
    insta::assert_snapshot!(
        format!("{:?}", AmpAdapter.observe_lifecycle("agent_start", &json!("junk"))),
        @"None"
    );
}

#[test]
fn install_preview_drift_and_uninstall_only_touch_managed_source() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugins/rimz.ts");
    let events = || {
        WIRED_EVENTS
            .iter()
            .map(|event| (*event).to_owned())
            .collect::<Vec<_>>()
    };

    let installed = AMP_MANAGED_SOURCE.install_into(&path).unwrap();
    assert!(!installed.merged);
    assert_eq!(installed.installed_events, events());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);
    assert!(AMP_MANAGED_SOURCE.installed_at(&path));

    std::fs::write(&path, "// stale _rimz_managed plugin\n").unwrap();
    let preview = AMP_MANAGED_SOURCE.preview_at(&path).unwrap();
    assert!(preview.merged);
    assert_eq!(preview.candidate_config, PLUGIN_SOURCE);
    AMP_MANAGED_SOURCE.install_into(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), PLUGIN_SOURCE);

    let removed = AMP_MANAGED_SOURCE.uninstall_from(&path).unwrap();
    assert_eq!(removed.removed_events, events());
    assert!(!path.exists());

    let user_path = dir.path().join("user.ts");
    std::fs::write(&user_path, "// user plugin\n").unwrap();
    assert!(matches!(
        AMP_MANAGED_SOURCE.install_into(&user_path),
        Err(AgentErr::Install { agent: "amp", .. })
    ));
    assert!(matches!(
        AMP_MANAGED_SOURCE.preview_at(&user_path),
        Err(AgentErr::Install { agent: "amp", .. })
    ));
    assert!(
        AMP_MANAGED_SOURCE
            .uninstall_from(&user_path)
            .unwrap()
            .removed_events
            .is_empty()
    );
    assert_eq!(
        std::fs::read_to_string(&user_path).unwrap(),
        "// user plugin\n"
    );
}

#[test]
fn plugin_source_pins_active_thread_observation_wire_and_pid() {
    assert!(
        PLUGIN_SOURCE
            .lines()
            .next()
            .unwrap()
            .contains("_rimz_managed")
    );
    assert!(PLUGIN_SOURCE.contains("\"hooks\", \"feed\", \"--source\", \"amp\""));
    assert!(PLUGIN_SOURCE.contains("RIMZ_AGENT_PID"));
    assert!(PLUGIN_SOURCE.contains("amp.activeThread.current"));
    assert!(PLUGIN_SOURCE.contains("awaiting-approval"));
    assert!(PLUGIN_SOURCE.contains("filesModifiedByToolCall"));
    assert!(PLUGIN_SOURCE.contains("sendQueue = sendQueue.then"));
    assert!(PLUGIN_SOURCE.contains("lastAssistantMessage(event.messages)"));
    assert!(PLUGIN_SOURCE.contains("files == null ? undefined"));
    assert!(!PLUGIN_SOURCE.contains("amp.on(\"tool.call\""));
    for event in ["session.start", "agent.start", "tool.result", "agent.end"] {
        assert!(PLUGIN_SOURCE.contains(event), "plugin missing {event}");
    }
}
