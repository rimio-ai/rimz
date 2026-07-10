use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::agents::{AgentHookClass, AgentStatus, LaunchPreset, PresetErr};

#[test]
fn install_preview_reclaim_drift_and_uninstall_preserve_user_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{
          "model": "custom",
          "hooks": {
            "Notification": [
              { "hooks": [{ "type": "command", "command": "echo user" }] },
              { "hooks": [{ "type": "command", "command": "rimz hooks feed --source droid --event Notification" }] }
            ]
          }
        }"#,
    )
    .unwrap();

    let before = std::fs::read_to_string(&path).unwrap();
    let preview = preview_install_at(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    let report = install_into(&path).unwrap();
    assert!(report.merged);
    assert_eq!(
        preview.candidate_config,
        std::fs::read_to_string(&path).unwrap()
    );
    assert!(hooks_installed_at(&path));

    let mut root: Value = serde_json::from_str(&preview.candidate_config).unwrap();
    let notification = root["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(notification.len(), 2, "one user hook plus one managed hook");
    assert_eq!(root["model"], "custom");
    root["hooks"].as_object_mut().unwrap().remove("Stop");
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(!hooks_installed_at(&path));
    install_into(&path).unwrap();
    assert!(hooks_installed_at(&path));

    let uninstall = uninstall_from(&path).unwrap();
    assert!(uninstall.existed);
    let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(root["model"], "custom");
    assert_eq!(root["hooks"]["Notification"].as_array().unwrap().len(), 1);
    assert_eq!(
        root["hooks"]["Notification"][0]["hooks"][0]["command"],
        "echo user"
    );
}

#[test]
fn lifecycle_maps_basic_turn_tools_compaction_and_end() {
    let startup = DroidAdapter
        .observe_lifecycle(
            "SessionStart",
            &json!({
                "session_id": "sess-1",
                "transcript_path": "/tmp/droid.jsonl",
                "cwd": "/tmp/project",
                "source": "startup"
            }),
        )
        .unwrap();
    assert_eq!(startup.signal, LifecycleSignal::Registered);
    assert_eq!(startup.origin, Some(SessionOrigin::Fresh));
    assert_eq!(startup.agent_id.as_deref(), Some("sess-1"));
    assert_eq!(startup.transcript_path.as_deref(), Some("/tmp/droid.jsonl"));

    let prompt = DroidAdapter
        .observe_lifecycle(
            "UserPromptSubmit",
            &json!({"session_id": "sess-1", "prompt": "  fix auth  "}),
        )
        .unwrap();
    assert_eq!(prompt.signal, LifecycleSignal::TurnStarted);
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    let running = step(None, &prompt.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);

    for (tool, mutates, edits) in [
        ("Edit", true, true),
        ("Execute", true, false),
        ("Read", false, false),
    ] {
        assert_eq!(
            DroidAdapter
                .observe_lifecycle(
                    "PostToolUse",
                    &json!({"session_id": "sess-1", "tool_name": tool}),
                )
                .unwrap()
                .signal,
            LifecycleSignal::ToolUsed { mutates, edits }
        );
    }

    let stop = DroidAdapter
        .observe_lifecycle("Stop", &json!({"session_id": "sess-1"}))
        .unwrap();
    assert_eq!(
        stop.signal,
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false
        }
    );
    let prior = LifecycleState {
        status: AgentStatus::Running,
        phase: TurnPhase::Reasoning,
        compacting: false,
    };
    assert_eq!(
        step(Some(&prior), &stop.signal).next.status,
        AgentStatus::Success
    );

    assert_eq!(
        DroidAdapter
            .observe_lifecycle("PreCompact", &json!({"session_id": "sess-1"}))
            .unwrap()
            .signal,
        LifecycleSignal::Compacting
    );
    assert_eq!(
        DroidAdapter
            .observe_lifecycle(
                "SessionStart",
                &json!({"session_id": "sess-1", "source": "compact"}),
            )
            .unwrap()
            .signal,
        LifecycleSignal::CompactionEnded { auto: None }
    );
    assert!(DroidAdapter.ends_session("SessionEnd"));
    assert_eq!(
        DroidAdapter
            .observe_lifecycle("SessionEnd", &json!({"session_id": "sess-1"}))
            .unwrap()
            .signal,
        LifecycleSignal::Ended
    );
}

#[test]
fn neutral_malformed_pid_and_launch_surfaces_are_explicit() {
    assert_eq!(
        DroidAdapter.classify_hook("Notification", &json!({})).class,
        AgentHookClass::Lifecycle
    );
    insta::assert_json_snapshot!(DroidAdapter.render_neutral("Stop").unwrap(), @"null");
    insta::assert_json_snapshot!(
        DroidAdapter.observe_lifecycle("Stop", &json!([])).unwrap(),
        @r###"
        {
          "signal": {
            "signal": "turn_ended",
            "errored": false,
            "parked_on_background": false
          }
        }
        "###
    );

    let descriptor = DroidAdapter.descriptor();
    assert!(descriptor.runs_as("droid"));
    assert!(descriptor.runs_as("droid-aarch64-unknown-linux-gnu"));
    assert!(!descriptor.runs_as("node"));
    assert_eq!(
        DroidAdapter.launch_command(&["--model".to_owned(), "glm".to_owned()], Some("review")),
        Some(vec![
            "droid".to_owned(),
            "--model".to_owned(),
            "glm".to_owned(),
            "--".to_owned(),
            "review".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter.resume_command("sess-1", Path::new("/tmp")),
        Some(vec![
            "droid".to_owned(),
            "--resume".to_owned(),
            "sess-1".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter.fork_command("sess-1", Path::new("/tmp")),
        Some(vec![
            "droid".to_owned(),
            "--fork".to_owned(),
            "sess-1".to_owned()
        ])
    );
    for mode in [
        PermissionMode::Auto,
        PermissionMode::Ask,
        PermissionMode::Yolo,
        PermissionMode::Plan,
    ] {
        assert!(DroidAdapter.permission_args(mode).is_empty());
    }

    assert_eq!(
        DroidAdapter.render_preset(&LaunchPreset {
            model: Some("glm-5".to_owned()),
            append_system_prompt_file: Some(Path::new("/tmp/append.md").to_path_buf()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "glm-5".to_owned(),
            "--append-system-prompt-file".to_owned(),
            "/tmp/append.md".to_owned()
        ])
    );
    assert_eq!(
        DroidAdapter.render_preset(&LaunchPreset {
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "droid",
            field: "effort"
        })
    );
}
