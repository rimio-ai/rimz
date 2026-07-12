use super::*;

use std::ffi::OsStr;

use crate::agents::lifecycle::{TurnPhase, step};
use crate::agents::{AgentErr, AgentStatus, LaunchPreset};
use serde_json::json;

#[test]
fn lifecycle_maps_through_the_shared_state_machine() {
    let registered = observed("SessionStart", json!({ "session_id": "s", "cwd": "/work" }));
    assert_eq!(registered.agent_id.as_deref(), Some("s"));
    assert_eq!(registered.worktree_path.as_deref(), Some("/work"));
    let idle = step(None, &registered.signal).next;
    assert_eq!(idle.status, AgentStatus::Idle);

    let prompt = observed(
        "UserPromptSubmit",
        json!({ "sessionId": "s", "userPrompt": "  fix auth  " }),
    );
    assert_eq!(prompt.prompt.as_deref(), Some("fix auth"));
    assert_eq!(prompt.task.as_deref(), Some("fix auth"));
    let running = step(Some(&idle), &prompt.signal).next;
    assert_eq!(running.status, AgentStatus::Running);
    assert_eq!(running.phase, TurnPhase::Reasoning);

    let edit = observed(
        "PostToolUse",
        json!({ "session_id": "s", "tool_name": "fs_write" }),
    );
    let acting = step(Some(&running), &edit.signal).next;
    assert_eq!(acting.status, AgentStatus::Running);
    assert_eq!(acting.phase, TurnPhase::Acting);

    let clean = observed("Stop", json!({ "session_id": "s" }));
    assert_eq!(
        step(Some(&acting), &clean.signal).next.status,
        AgentStatus::Success
    );
    let failed = observed("Stop", json!({ "session_id": "s", "is_error": true }));
    assert_eq!(
        step(Some(&acting), &failed.signal).next.status,
        AgentStatus::Failed
    );
}

#[test]
fn unknown_tools_and_malformed_payloads_stay_silent_and_safe() {
    for payload in [
        json!({ "session_id": "s", "tool_name": "fs_read" }),
        json!({ "session_id": "s" }),
        json!({ "session_id": "s", "tool_name": 7 }),
        serde_json::Value::Null,
    ] {
        assert!(
            KiroAdapter
                .observe_lifecycle("PostToolUse", &payload)
                .is_none()
        );
    }

    let idless = observed("SessionStart", json!({ "cwd": "/work" }));
    assert!(idless.agent_id.is_none());
    assert_eq!(
        observed("SessionStart", json!({ "session_id": "  session-1  " }))
            .agent_id
            .as_deref(),
        Some("session-1")
    );
    assert!(
        observed("SessionStart", json!({ "session_id": "   " }))
            .agent_id
            .is_none()
    );
    assert!(
        observed("UserPromptSubmit", json!({ "prompt": 7 }))
            .prompt
            .is_none()
    );
    assert!(
        KiroAdapter
            .observe_lifecycle("unknown", &json!(null))
            .is_none()
    );
}

#[test]
fn installed_events_render_empty_stdout() {
    let rendered: Vec<_> = WIRED_EVENTS
        .iter()
        .map(|event| (*event, KiroAdapter.render_neutral(event).unwrap()))
        .collect();
    insta::assert_debug_snapshot!(rendered, @r###"
    [
        (
            "SessionStart",
            None,
        ),
        (
            "UserPromptSubmit",
            None,
        ),
        (
            "PostToolUse",
            None,
        ),
        (
            "Stop",
            None,
        ),
    ]
    "###);
}

#[test]
fn install_preview_drift_and_uninstall_preserve_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");

    let preview = install::preview_at(&path).unwrap();
    assert_eq!(preview.planned_events, WIRED_EVENTS);
    assert!(!preview.merged);
    assert!(!path.exists());

    let first = install::install_into(&path).unwrap();
    assert!(!first.merged);
    let canonical = std::fs::read(&path).unwrap();
    assert!(install::installed_at(&path));
    assert!(install::managed_at(&path));

    let second = install::install_into(&path).unwrap();
    assert!(second.merged);
    assert_eq!(std::fs::read(&path).unwrap(), canonical);

    let mut drift: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    drift["hooks"].as_array_mut().unwrap().pop();
    std::fs::write(&path, serde_json::to_vec_pretty(&drift).unwrap()).unwrap();
    assert!(!install::installed_at(&path));
    assert!(install::managed_at(&path));

    let mut disabled: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    disabled["hooks"][0]["enabled"] = json!(false);
    std::fs::write(&path, serde_json::to_vec_pretty(&disabled).unwrap()).unwrap();
    assert!(!install::installed_at(&path));
    assert!(install::managed_at(&path));

    let mut schema_drift: serde_json::Value = serde_json::from_slice(&canonical).unwrap();
    schema_drift["hooks"][0]["description"] = json!("locally edited");
    std::fs::write(&path, serde_json::to_vec_pretty(&schema_drift).unwrap()).unwrap();
    assert!(!install::installed_at(&path));
    assert!(install::managed_at(&path));

    let removed = install::uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(removed.removed_events, WIRED_EVENTS);
    assert!(!path.exists());

    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"version":"v1","hooks":[]}"#).unwrap();
    assert!(matches!(
        install::install_into(&path),
        Err(AgentErr::Install { .. })
    ));
    let untouched = std::fs::read(&path).unwrap();
    assert!(
        install::uninstall_from(&path)
            .unwrap()
            .removed_events
            .is_empty()
    );
    assert_eq!(std::fs::read(&path).unwrap(), untouched);
}

#[test]
fn candidate_config_is_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let preview = install::preview_at(&dir.path().join("rimz.json")).unwrap();
    let executable = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let stable = preview
        .candidate_config
        .replace(&executable, "/path/to/rimz");
    insta::assert_snapshot!(stable, @r###"
    {
      "version": "v1",
      "hooks": [
        {
          "trigger": "SessionStart",
          "name": "rimz-session-start",
          "action": {
            "type": "command",
            "command": "/path/to/rimz hooks feed --source kiro --event SessionStart"
          },
          "timeout": 10,
          "enabled": true
        },
        {
          "trigger": "UserPromptSubmit",
          "name": "rimz-user-prompt-submit",
          "action": {
            "type": "command",
            "command": "/path/to/rimz hooks feed --source kiro --event UserPromptSubmit"
          },
          "timeout": 10,
          "enabled": true
        },
        {
          "trigger": "PostToolUse",
          "name": "rimz-post-tool-use",
          "action": {
            "type": "command",
            "command": "/path/to/rimz hooks feed --source kiro --event PostToolUse"
          },
          "timeout": 10,
          "enabled": true
        },
        {
          "trigger": "Stop",
          "name": "rimz-stop",
          "action": {
            "type": "command",
            "command": "/path/to/rimz hooks feed --source kiro --event Stop"
          },
          "timeout": 10,
          "enabled": true
        }
      ]
    }
    "###);
}

#[test]
fn hooks_path_prefers_override_then_kiro_home() {
    let override_path = OsStr::new("/tmp/override.json");
    let kiro_home = OsStr::new("/tmp/kiro");
    let home = OsStr::new("/home/user");
    assert_eq!(
        install::resolve_hooks_path(Some(override_path), Some(kiro_home), Some(home)).unwrap(),
        std::path::PathBuf::from("/tmp/override.json")
    );
    assert_eq!(
        install::resolve_hooks_path(None, Some(kiro_home), Some(home)).unwrap(),
        std::path::PathBuf::from("/tmp/kiro/hooks/rimz.json")
    );
    assert_eq!(
        install::resolve_hooks_path(None, None, Some(home)).unwrap(),
        std::path::PathBuf::from("/home/user/.kiro/hooks/rimz.json")
    );
    assert!(matches!(
        install::resolve_hooks_path(None, None, None),
        Err(AgentErr::Install { .. })
    ));
}

#[test]
fn launch_resume_and_presets_use_v3_surface() {
    assert_eq!(
        KiroAdapter.launch_command(
            &["--agent".to_owned(), "reviewer".to_owned()],
            Some("review")
        ),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--agent".to_owned(),
            "reviewer".to_owned(),
            "--".to_owned(),
            "review".to_owned(),
        ])
    );
    assert_eq!(
        KiroAdapter.resume_command("session-1", Path::new("/work")),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--resume-id".to_owned(),
            "session-1".to_owned(),
        ])
    );
    assert!(
        KiroAdapter
            .fork_command("session-1", Path::new("/work"))
            .is_none()
    );
    assert_eq!(
        KiroAdapter.render_preset(&LaunchPreset {
            model: Some("auto".to_owned()),
            effort: Some("high".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "auto".to_owned(),
            "--effort".to_owned(),
            "high".to_owned()
        ])
    );
}

#[test]
fn presence_matches_launcher_and_chat_engine() {
    let descriptor = KiroAdapter.descriptor();
    // The launcher and the v3 chat engine both read as Kiro.
    assert!(descriptor.runs_as("kiro-cli"));
    assert!(descriptor.runs_as("kiro-cli-chat"));
    // The figterm shell-integration daemon runs for every integrated shell, so
    // it must never bind a pane as an agent.
    assert!(!descriptor.runs_as("kiro-cli-term"));
}

fn observed(event: &str, payload: serde_json::Value) -> AgentLifecycleObservation {
    KiroAdapter
        .observe_lifecycle(event, &payload)
        .expect("observation")
}
