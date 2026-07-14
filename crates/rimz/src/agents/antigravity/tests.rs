use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;

use serde_json::json;

use super::*;
use crate::agents::{
    AgentErr, AgentStatus, LaunchPreset, TranscriptPosition, TranscriptRole, TurnPhase,
};

const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

#[test]
fn native_observer_hooks_are_explicitly_unsupported() {
    let descriptor = AntigravityAdapter.descriptor();
    assert!(!descriptor.capabilities.hook_install);
    assert!(!descriptor.capabilities.blocking_asks);
    assert!(descriptor.activity_events.is_empty());
    assert!(AntigravityAdapter.installed_hook_events().is_empty());

    for event in [
        "PreToolUse",
        "PostToolUse",
        "PreInvocation",
        "PostInvocation",
        "Stop",
    ] {
        let payload = json!({ "conversationId": SESSION_ID });
        assert_eq!(
            AntigravityAdapter.classify_hook(event, &payload).class,
            AgentHookClass::Unknown
        );
        assert!(
            AntigravityAdapter
                .observe_lifecycle(event, &payload)
                .is_none()
        );
        assert_eq!(AntigravityAdapter.render_neutral(event).unwrap(), None);
    }

    for result in [
        AntigravityAdapter.install_hooks().map(|_| ()),
        AntigravityAdapter.preview_hook_install().map(|_| ()),
    ] {
        let error = result.unwrap_err();
        assert!(matches!(error, AgentErr::Install { .. }));
        assert!(error.to_string().contains("observer hooks are deferred"));
    }
}

#[test]
fn verified_visible_transcript_records_are_normalized_strictly() {
    let transcript = include_str!("tests/fixtures/transcript.jsonl");
    let messages = AntigravityAdapter.parse_transcript_messages(transcript);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong");
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("checkpoint"))
    );

    assert!(
        AntigravityAdapter
            .parse_transcript_messages("not-json\n{}")
            .is_empty()
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_THOUGHT","status":"DONE","created_at":"2026-07-13T23:23:10Z","content":"hidden"}"#,
            )
            .is_empty()
    );
    assert!(
        AntigravityAdapter
            .parse_transcript_messages(
                r#"{"step_index":4,"source":"MODEL","type":"PLANNER_RESPONSE","status":"IN_PROGRESS","created_at":"2026-07-13T23:23:10Z","content":"partial"}"#,
            )
            .is_empty()
    );
}

#[test]
fn transcript_cursor_retains_a_torn_final_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("transcript.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(
        b"{\"step_index\":0,\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:09Z\",\"content\":\"one\"}\n{\"step_index\":1",
    )
    .unwrap();
    let page = AntigravityAdapter
        .read_assistant_transcript_page(&path, None, TranscriptPosition::START)
        .unwrap();
    assert_eq!(page.messages, ["one"]);
    let next = page.next;
    file.write_all(
        b",\"source\":\"MODEL\",\"type\":\"PLANNER_RESPONSE\",\"status\":\"DONE\",\"created_at\":\"2026-07-13T23:23:10Z\",\"content\":\"two\"}",
    )
    .unwrap();
    let page = AntigravityAdapter
        .read_assistant_transcript_page(&path, None, next)
        .unwrap();
    assert_eq!(page.messages, ["two"]);
}

#[test]
fn discovery_uses_cache_only_for_fresh_pairing_and_keeps_exact_resume_available() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let transcript = write_transcript(dir.path(), SESSION_ID);
    std::fs::create_dir_all(dir.path().join("cache")).unwrap();
    std::fs::write(
        dir.path().join("cache/last_conversations.json"),
        format!(
            "{{{}:{}}}",
            serde_json::to_string(&workspace).unwrap(),
            serde_json::to_string(SESSION_ID).unwrap()
        ),
    )
    .unwrap();

    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(observation.session_id.as_str(), SESSION_ID);
    assert_eq!(observation.transcript_path, transcript);
    assert_eq!(observation.status, AgentStatus::Success);
    assert_eq!(observation.phase, TurnPhase::Idle);
    assert_eq!(observation.latest_prompt.as_deref(), Some("ping"));
    assert!(observation.first_event_at.is_some());

    let other_workspace = dir.path().join("other");
    std::fs::create_dir(&other_workspace).unwrap();
    let observations = session::discover_under(dir.path(), &other_workspace);
    assert_eq!(observations.len(), 1);
    assert!(
        observations[0].first_event_at.is_none(),
        "an unrelated workspace can bind this record only by exact resume id"
    );
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_conversation_directories() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let escaped = tempfile::tempdir().unwrap();
    let escaped_transcript = escaped
        .path()
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(escaped_transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &escaped_transcript,
        include_str!("tests/fixtures/transcript.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("brain")).unwrap();
    symlink(escaped.path(), dir.path().join("brain").join(SESSION_ID)).unwrap();
    assert!(session::discover_under(dir.path(), Path::new("/workspace/project")).is_empty());
}

#[test]
fn launch_resume_permissions_and_model_preset_match_agy_1_1_1() {
    assert_eq!(SUPPORTED_VERSION, "1.1.1");
    assert_eq!(
        AntigravityAdapter.launch_command(&["--sandbox".to_owned()], Some("review")),
        Some(vec![
            "agy".to_owned(),
            "--sandbox".to_owned(),
            "--prompt-interactive".to_owned(),
            "review".to_owned(),
        ])
    );
    assert_eq!(
        AntigravityAdapter.resume_command(SESSION_ID, Path::new("/workspace/project")),
        Some(vec![
            "agy".to_owned(),
            "--conversation".to_owned(),
            SESSION_ID.to_owned(),
        ])
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Auto),
        ["--mode", "accept-edits"]
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Plan),
        ["--mode", "plan"]
    );
    assert_eq!(
        AntigravityAdapter.permission_args(PermissionMode::Yolo),
        ["--dangerously-skip-permissions"]
    );
    assert_eq!(
        AntigravityAdapter.render_preset(&LaunchPreset {
            model: Some("Gemini 3.5 Flash (Low)".to_owned()),
            ..Default::default()
        }),
        Ok(vec![
            "--model".to_owned(),
            "Gemini 3.5 Flash (Low)".to_owned(),
        ])
    );
    assert!(
        AntigravityAdapter
            .render_preset(&LaunchPreset {
                effort: Some("high".to_owned()),
                ..Default::default()
            })
            .is_err()
    );
    assert!(AntigravityAdapter.compact_command().is_none());
    assert!(
        AntigravityAdapter
            .fork_command(SESSION_ID, Path::new("/workspace"))
            .is_none()
    );
}

#[test]
fn exact_resume_parser_accepts_both_flag_forms_without_claiming_continue() {
    for command in [
        "agy --conversation 11111111-1111-4111-8111-111111111111",
        "/home/user/.local/bin/agy --conversation=11111111-1111-4111-8111-111111111111",
    ] {
        assert_eq!(
            AntigravityAdapter
                .resumed_session_id_from_cmdline(command)
                .as_deref(),
            Some(SESSION_ID)
        );
    }
    for command in [
        "agy --continue",
        "agy -c",
        "agy --conversation=",
        "agy --conversation --mode plan",
        "echo agy --conversation 11111111-1111-4111-8111-111111111111",
    ] {
        assert!(
            AntigravityAdapter
                .resumed_session_id_from_cmdline(command)
                .is_none()
        );
    }
}

#[test]
fn home_resolution_and_presence_are_exact() {
    assert_eq!(
        session::resolve_home(Some(OsStr::new("/tmp/agy")), Some(OsStr::new("/home/user"))),
        Some(Path::new("/tmp/agy").to_path_buf())
    );
    assert_eq!(
        session::resolve_home(None, Some(OsStr::new("/home/user"))),
        Some(Path::new("/home/user/.gemini/antigravity-cli").to_path_buf())
    );
    assert!(AntigravityAdapter.descriptor().runs_as("agy"));
    assert!(!AntigravityAdapter.descriptor().runs_as("antigravity"));
}

fn write_transcript(home: &Path, session_id: &str) -> std::path::PathBuf {
    let path = home
        .join("brain")
        .join(session_id)
        .join(".system_generated/logs/transcript.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, include_str!("tests/fixtures/transcript.jsonl")).unwrap();
    path
}
