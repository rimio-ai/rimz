use super::*;

use std::ffi::OsStr;
use std::io::Write as _;

use crate::agents::descriptor::{ConcernCoverage, IntegrationConcern};
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentErr, LaunchPreset, TranscriptPosition, TranscriptRole};
use serde_json::json;

#[test]
fn native_hooks_are_explicitly_unsupported() {
    let descriptor = KiroAdapter.descriptor();
    assert!(!descriptor.capabilities.hook_install);
    assert!(descriptor.activity_events.is_empty());
    assert!(KiroAdapter.installed_hook_events().is_empty());
    assert!(matches!(
        descriptor
            .coverage
            .iter()
            .find(|(concern, _)| *concern == IntegrationConcern::TurnLifecycle)
            .map(|(_, coverage)| *coverage),
        Some(ConcernCoverage::Partial { .. })
    ));

    for event in ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"] {
        let payload = json!({ "session_id": "sess_redacted", "prompt": "ignored" });
        assert_eq!(
            KiroAdapter.classify_hook(event, &payload).class,
            AgentHookClass::Unknown
        );
        assert!(KiroAdapter.observe_lifecycle(event, &payload).is_none());
        assert_eq!(KiroAdapter.render_neutral(event).unwrap(), None);
    }

    let observation =
        super::super::AgentLifecycleObservation::new(None, LifecycleSignal::Registered);
    assert!(
        KiroAdapter
            .last_assistant_message("Stop", &json!({}), &observation)
            .is_none()
    );
}

#[test]
fn stock_store_transcript_context_and_lifecycle_are_normalized() {
    let descriptor = KiroAdapter.descriptor();
    assert!(descriptor.capabilities.local_session_discovery);
    assert!(descriptor.capabilities.transcript_tail_context);
    assert!(descriptor.capabilities.context_usage);
    assert!(!descriptor.capabilities.rich_context);
    assert!(!descriptor.capabilities.account_spend);
    assert!(
        !descriptor
            .capabilities
            .realtime_usage
            .covers_account_while_live
    );

    let ping = include_str!("tests/fixtures/stock_ping/messages.jsonl");
    let messages = KiroAdapter.parse_transcript_messages(ping);
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, TranscriptRole::User);
    assert_eq!(messages[0].text, "ping");
    assert_eq!(messages[1].role, TranscriptRole::Assistant);
    assert_eq!(messages[1].text, "pong");
    assert!(
        messages
            .iter()
            .all(|message| !message.text.contains("bootstrap"))
    );

    let history = include_str!("tests/fixtures/root/sess_redacted.history");
    let acp = include_str!("tests/fixtures/acp/11111111-1111-4111-8111-111111111111.jsonl");
    assert!(KiroAdapter.parse_transcript_messages(history).is_empty());
    assert!(KiroAdapter.parse_transcript_messages(acp).is_empty());
}

#[test]
fn discovery_validates_layout_and_folds_ordered_records() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let bucket = dir
        .path()
        .join("sessions")
        .join(session::workspace_bucket(&workspace).unwrap());
    let session_dir = bucket.join("sess_11111111-1111-4111-8111-111111111111");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.json"),
        format!(
            r#"{{"id":"sess_11111111-1111-4111-8111-111111111111","schemaVersion":"1.0.0","dataModelVersion":1,"workspacePaths":[{}],"createdAt":"2025-01-01T00:00:00Z","status":"idle"}}"#,
            serde_json::to_string(&workspace).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        session_dir.join("messages.jsonl"),
        include_str!("tests/fixtures/stock_ping/messages.jsonl"),
    )
    .unwrap();

    let observations = session::discover_under(dir.path(), &workspace);
    assert_eq!(observations.len(), 1);
    let observation = &observations[0];
    assert_eq!(
        observation.session_id.as_str(),
        "sess_11111111-1111-4111-8111-111111111111"
    );
    assert_eq!(observation.status, crate::agents::AgentStatus::Success);
    assert_eq!(observation.phase, crate::agents::TurnPhase::Idle);
    assert_eq!(observation.latest_prompt.as_deref(), Some("ping"));
    assert_eq!(observation.context_pct, Some(13));

    std::fs::write(
        session_dir.join("session.json"),
        r#"{"id":"sess_11111111-1111-4111-8111-111111111111","schemaVersion":"2.0.0","dataModelVersion":1,"workspacePaths":[],"createdAt":"2025-01-01T00:00:00Z","status":"idle"}"#,
    )
    .unwrap();
    assert!(session::discover_under(dir.path(), &workspace).is_empty());
}

#[cfg(unix)]
#[test]
fn discovery_rejects_symlinked_workspace_buckets() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let bucket_name = session::workspace_bucket(&workspace).unwrap();
    let escaped = dir.path().join("escaped").join(&bucket_name);
    let session_dir = escaped.join("sess_11111111-1111-4111-8111-111111111111");
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::write(
        session_dir.join("session.json"),
        format!(
            r#"{{"id":"sess_11111111-1111-4111-8111-111111111111","schemaVersion":"1.0.0","dataModelVersion":1,"workspacePaths":[{}],"createdAt":"2025-01-01T00:00:00Z","status":"idle"}}"#,
            serde_json::to_string(&workspace).unwrap()
        ),
    )
    .unwrap();
    std::fs::write(
        session_dir.join("messages.jsonl"),
        include_str!("tests/fixtures/stock_ping/messages.jsonl"),
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("sessions")).unwrap();
    symlink(&escaped, dir.path().join("sessions").join(bucket_name)).unwrap();

    assert!(session::discover_under(dir.path(), &workspace).is_empty());
}

#[test]
fn approval_waiting_resolution_tool_activity_and_context_clamp_follow_file_order() {
    let lines = include_str!("tests/fixtures/stock_approval/messages.jsonl");
    let pending_end = lines.find("{\"id\":\"context-2a\"").unwrap();
    let pending = session::fold_for_test(&lines[..pending_end]);
    assert_eq!(pending.0, crate::agents::AgentStatus::Waiting);
    assert_eq!(pending.2.as_deref(), Some("Write File"));

    let tool_end = lines.find("{\"id\":\"tool-2-result\"").unwrap();
    let acting = session::fold_for_test(&lines[..tool_end]);
    assert_eq!(acting.0, crate::agents::AgentStatus::Running);
    assert_eq!(acting.1, crate::agents::TurnPhase::Acting);

    let settled = session::fold_for_test(lines);
    assert_eq!(settled.0, crate::agents::AgentStatus::Success);
    assert_eq!(settled.1, crate::agents::TurnPhase::Idle);
    assert_eq!(settled.3, Some(100));
}

#[test]
fn transcript_cursor_retains_torn_final_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("messages.jsonl");
    let mut file = std::fs::File::create(&path).unwrap();
    file.write_all(b"{\"id\":\"a\",\"timestamp\":\"2025-01-01T00:00:00Z\",\"payload\":{\"type\":\"assistant\",\"operationType\":\"Say\",\"content\":\"one\"}}\n{\"id\":\"b\"").unwrap();
    let page = KiroAdapter
        .read_assistant_transcript_page(&path, None, TranscriptPosition::START)
        .unwrap();
    assert_eq!(page.messages, ["one"]);
    let next = page.next;
    file.write_all(b",\"timestamp\":\"2025-01-01T00:00:01Z\",\"payload\":{\"type\":\"assistant\",\"operationType\":\"Say\",\"content\":\"two\"}}").unwrap();
    let page = KiroAdapter
        .read_assistant_transcript_page(&path, None, next)
        .unwrap();
    assert_eq!(page.messages, ["two"]);
}

#[test]
fn workspace_hash_and_resume_parser_are_exact() {
    assert_eq!(
        session::workspace_bucket(Path::new("/workspace/project")).as_deref(),
        Some("e3af8a7251583e76")
    );
    for command in [
        "kiro-cli chat --v3 --resume-id sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli --resume-id=sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli-chat --resume-id sess_11111111-1111-4111-8111-111111111111",
    ] {
        assert_eq!(
            KiroAdapter
                .resumed_session_id_from_cmdline(command)
                .as_deref(),
            Some("sess_11111111-1111-4111-8111-111111111111")
        );
    }
    for command in [
        "kiro-cli-term --resume-id sess_11111111-1111-4111-8111-111111111111",
        "kiro-cli --resume-id unrelated",
        "echo --resume-id sess_11111111-1111-4111-8111-111111111111",
    ] {
        assert!(
            KiroAdapter
                .resumed_session_id_from_cmdline(command)
                .is_none()
        );
    }
}

#[test]
fn hook_install_refuses_but_legacy_owned_files_can_be_removed() {
    for result in [
        KiroAdapter.install_hooks().map(|_| ()),
        KiroAdapter.preview_hook_install().map(|_| ()),
    ] {
        let err = result.expect_err("Kiro v3 hook install must fail");
        assert!(
            err.to_string()
                .contains("does not execute standalone hook configs")
        );
    }
    assert!(!KiroAdapter.hooks_installed());

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        r#"{"version":"v1","hooks":[{"action":{"command":"/old/rimz hooks feed --source kiro --event Stop"}}]}"#,
    )
    .unwrap();
    assert!(install::managed_at(&path));
    let removed = install::uninstall_from(&path).unwrap();
    assert!(removed.existed);
    assert_eq!(
        removed.removed_events,
        ["SessionStart", "UserPromptSubmit", "PostToolUse", "Stop"]
    );
    assert!(!path.exists());
}

#[test]
fn legacy_cleanup_preserves_unowned_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("hooks/rimz.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let user_config = r#"{"version":"v1","hooks":[{"action":{"command":"my-hook"}}]}"#;
    std::fs::write(&path, user_config).unwrap();

    assert!(!install::managed_at(&path));
    let report = install::uninstall_from(&path).unwrap();
    assert!(report.existed);
    assert!(report.removed_events.is_empty());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), user_config);
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
    assert_eq!(
        install::resolve_home(Some(kiro_home), Some(home)).unwrap(),
        std::path::PathBuf::from("/tmp/kiro")
    );
    assert_eq!(
        install::resolve_home(None, Some(home)).unwrap(),
        std::path::PathBuf::from("/home/user/.kiro")
    );
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
        KiroAdapter.resume_command("sess_redacted", Path::new("/work")),
        Some(vec![
            "kiro-cli".to_owned(),
            "chat".to_owned(),
            "--v3".to_owned(),
            "--resume-id".to_owned(),
            "sess_redacted".to_owned(),
        ])
    );
    assert!(
        KiroAdapter
            .fork_command("sess_redacted", Path::new("/work"))
            .is_none()
    );
    assert_eq!(KiroAdapter.compact_command(), Some("/compact"));
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
    assert!(descriptor.runs_as("kiro-cli"));
    assert!(descriptor.runs_as("kiro-cli-chat"));
    assert!(!descriptor.runs_as("kiro-cli-term"));
}
