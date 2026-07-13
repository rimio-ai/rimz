use super::*;

use std::ffi::OsStr;

use crate::agents::descriptor::{ConcernCoverage, IntegrationConcern};
use crate::agents::lifecycle::LifecycleSignal;
use crate::agents::{AgentErr, LaunchPreset};
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
        Some(ConcernCoverage::Unsupported { .. })
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
fn transcript_context_and_spend_surfaces_remain_absent() {
    let descriptor = KiroAdapter.descriptor();
    assert!(!descriptor.capabilities.transcript_tail_context);
    assert!(!descriptor.capabilities.context_usage);
    assert!(!descriptor.capabilities.rich_context);
    assert!(!descriptor.capabilities.account_spend);
    assert!(
        !descriptor
            .capabilities
            .realtime_usage
            .covers_account_while_live
    );

    let history = include_str!("tests/fixtures/root/sess_redacted.history");
    let acp = include_str!("tests/fixtures/acp/11111111-1111-4111-8111-111111111111.jsonl");
    assert!(KiroAdapter.parse_transcript_messages(history).is_empty());
    assert!(KiroAdapter.parse_transcript_messages(acp).is_empty());
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
