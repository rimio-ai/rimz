use std::io::Write as _;
use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::agents::lifecycle::{LifecycleState, TurnPhase, step};
use crate::agents::transcript::TranscriptCursor;
use crate::agents::{
    AgentHookClass, AgentStatus, LaunchPreset, PresetErr, TranscriptPosition, TranscriptRole,
};

const TRANSCRIPT_FIXTURE: &str = include_str!("tests/fixtures/droid-0.170.0-transcript-v2.jsonl");
const SETTINGS_FIXTURE: &str =
    include_str!("tests/fixtures/droid-0.170.0-transcript-v2.settings.json");

fn transcript_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, TRANSCRIPT_FIXTURE).unwrap();
    std::fs::write(dir.path().join("session.settings.json"), SETTINGS_FIXTURE).unwrap();
    (dir, path)
}

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
    assert!(report.files[0].existed);
    assert_eq!(
        preview.files[0].candidate,
        std::fs::read_to_string(&path).unwrap()
    );
    assert!(hooks_installed_at(&path));

    let mut root: Value = serde_json::from_str(&preview.files[0].candidate).unwrap();
    let notification = root["hooks"]["Notification"].as_array().unwrap();
    assert_eq!(notification.len(), 2, "one user hook plus one managed hook");
    assert_eq!(root["model"], "custom");
    root["hooks"].as_object_mut().unwrap().remove("Stop");
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(!hooks_installed_at(&path));
    install_into(&path).unwrap();
    assert!(hooks_installed_at(&path));

    let mut root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    root["hooks"]["PostToolUse"][0]["hooks"][0]["timeout"] = json!(60);
    std::fs::write(&path, serde_json::to_string_pretty(&root).unwrap()).unwrap();
    assert!(
        !hooks_installed_at(&path),
        "timeout drift must re-offer the canonical hook merge"
    );
    install_into(&path).unwrap();
    assert!(hooks_installed_at(&path));

    let uninstall = uninstall_from(&path).unwrap();
    assert!(uninstall.files[0].existed);
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
fn transcript_v2_follows_active_chain_and_filters_private_blocks() {
    let messages = DroidAdapter.parse_transcript_messages(TRANSCRIPT_FIXTURE);

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong\nsecond block");
    assert_eq!(
        messages[0].at.map(|at| at.to_string()).as_deref(),
        Some("2026-07-13T20:19:51.315Z")
    );
    assert!(messages.iter().all(|message| {
        !message.text.contains("hidden")
            && !message.text.contains("abandoned")
            && !message.text.contains("hook")
    }));
}

#[test]
fn transcript_v2_abstains_on_unknown_version_and_malformed_graphs() {
    let unknown = TRANSCRIPT_FIXTURE.replacen("\"version\":2", "\"version\":3", 1);
    assert!(DroidAdapter.parse_transcript_messages(&unknown).is_empty());

    let missing_parent = concat!(
        "{\"type\":\"session_start\",\"version\":2}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":\"missing\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"nope\"}]}}\n",
    );
    assert!(
        DroidAdapter
            .parse_transcript_messages(missing_parent)
            .is_empty()
    );

    let cycle = concat!(
        "{\"type\":\"session_start\",\"version\":2}\n",
        "{\"type\":\"message\",\"id\":\"a\",\"parentId\":\"b\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"one\"}]}}\n",
        "{\"type\":\"message\",\"id\":\"b\",\"parentId\":\"a\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"two\"}]}}\n",
    );
    assert!(DroidAdapter.parse_transcript_messages(cycle).is_empty());
}

#[test]
fn final_answer_and_identity_are_version_gated_and_bounded() {
    let (_dir, path) = transcript_fixture();
    let observation = DroidAdapter
        .observe_lifecycle(
            "Stop",
            &json!({
                "session_id": "sess-1",
                "transcript_path": path,
            }),
        )
        .unwrap();

    assert_eq!(
        observation.launch.model.as_deref(),
        Some("custom:settings-model")
    );
    assert_eq!(observation.launch.effort.as_deref(), Some("medium"));
    assert_eq!(observation.context_pct, None);
    assert_eq!(observation.context_window, None);
    assert_eq!(observation.total_tokens, None);
    assert_eq!(observation.fresh_input_tokens, None);
    assert_eq!(observation.output_tokens, None);
    assert_eq!(
        DroidAdapter.last_assistant_message("Stop", &Value::Null, &observation),
        Some("pong\nsecond block".to_owned())
    );
    assert_eq!(
        DroidAdapter.last_assistant_message("SessionEnd", &Value::Null, &observation),
        None
    );

    std::fs::remove_file(path.with_file_name("session.settings.json")).unwrap();
    let fallback = DroidAdapter
        .observe_lifecycle(
            "Stop",
            &json!({"session_id": "sess-1", "transcript_path": path}),
        )
        .unwrap();
    assert_eq!(
        fallback.launch.model.as_deref(),
        Some("custom:fixture-model")
    );
    assert_eq!(fallback.launch.effort.as_deref(), Some("high"));
}

#[test]
fn suffix_streaming_is_exactly_once_torn_safe_and_resets_after_truncation() {
    let (_dir, path) = transcript_fixture();
    let path_text = path.to_string_lossy().into_owned();
    let mut cursor = TranscriptCursor::new(true);

    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["abandoned answer", "pong\nsecond block"]
    );
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(b"{\"type\":\"message\",\"id\":\"new\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"fi")
        .unwrap();
    file.flush().unwrap();
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );
    file.write_all(b"nal\"}]}}\n").unwrap();
    file.flush().unwrap();
    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["final"]
    );
    assert!(
        cursor
            .messages(Some(&path_text), None, &DroidAdapter)
            .is_empty()
    );

    std::fs::write(
        &path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"fresh\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"fresh\"}]}}\n",
        ),
    )
    .unwrap();
    assert_eq!(
        cursor.messages(Some(&path_text), None, &DroidAdapter),
        ["fresh"]
    );
}

#[test]
fn transcript_positions_abstain_for_missing_or_unknown_headers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    std::fs::write(&path, "{\"type\":\"session_start\",\"version\":9}\n").unwrap();

    assert_eq!(DroidAdapter.transcript_position(&path, None), None);
    assert_eq!(
        DroidAdapter.read_assistant_transcript_page(&path, None, TranscriptPosition::START),
        None
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
        DroidAdapter.launch_command(&["--auto".to_owned(), "medium".to_owned()], Some("review")),
        Some(vec![
            "droid".to_owned(),
            "--auto".to_owned(),
            "medium".to_owned(),
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
    assert_eq!(
        DroidAdapter.permission_args(PermissionMode::Auto),
        ["--auto", "medium"]
    );
    assert!(DroidAdapter.permission_args(PermissionMode::Ask).is_empty());
    assert!(
        DroidAdapter
            .permission_args(PermissionMode::Yolo)
            .is_empty()
    );
    assert_eq!(
        DroidAdapter.permission_args(PermissionMode::Plan),
        ["--use-spec"]
    );

    assert_eq!(
        DroidAdapter.render_preset(&LaunchPreset {
            append_system_prompt_file: Some(Path::new("/tmp/append.md").to_path_buf()),
            ..Default::default()
        }),
        Ok(vec![
            "--append-system-prompt-file".to_owned(),
            "/tmp/append.md".to_owned()
        ])
    );
    // Interactive Droid 0.170.0 has no `--model`/`--reasoning-effort`; both are
    // exec-only, so a profile that sets either fails fast rather than launching
    // with a silently ignored (and prompt-corrupting) flag.
    assert_eq!(
        DroidAdapter.render_preset(&LaunchPreset {
            model: Some("glm-5".to_owned()),
            ..Default::default()
        }),
        Err(PresetErr::UnsupportedField {
            agent: "droid",
            field: "model"
        })
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
