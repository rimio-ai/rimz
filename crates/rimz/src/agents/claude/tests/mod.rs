use super::*;
use crate::agents::TurnErrorClass;
use crate::feed::ResolutionMethod;
use crate::run::PermissionMode;
use serde_json::json;
use std::path::Path;

mod context;
mod decision;
mod install;
mod install_statusline;
mod lifecycle;

fn fixture(kind: FeedKind) -> FeedItem {
    crate::agents::testkit::feed_item(kind, "claude")
}

#[test]
fn claude_commands_and_permission_args_match_run_posture() {
    let argv = ClaudeAdapter
        .resume_command("sess-123", Path::new("/code/query-engine"))
        .expect("claude resumes");
    assert_eq!(argv, vec!["claude", "--resume", "sess-123"]);

    assert_eq!(
        ClaudeAdapter.launch_command(&[], None),
        Some(vec!["claude".to_owned()])
    );
    assert_eq!(
        ClaudeAdapter.launch_command(&[], Some("review this")),
        Some(vec!["claude".to_owned(), "review this".to_owned()])
    );
    assert_eq!(
        ClaudeAdapter.launch_command(
            &["--permission-mode".to_owned(), "plan".to_owned()],
            Some("review this")
        ),
        Some(vec![
            "claude".to_owned(),
            "--permission-mode".to_owned(),
            "plan".to_owned(),
            "review this".to_owned()
        ])
    );

    assert_eq!(
        ClaudeAdapter.permission_args(PermissionMode::Auto),
        vec!["--permission-mode", "auto"]
    );
    assert!(
        ClaudeAdapter
            .permission_args(PermissionMode::Ask)
            .is_empty()
    );
    assert_eq!(
        ClaudeAdapter.permission_args(PermissionMode::Yolo),
        vec!["--dangerously-skip-permissions"]
    );
}

#[test]
fn claude_render_preset_maps_model_effort_and_system_prompt_file() {
    let argv = ClaudeAdapter
        .render_preset(&crate::agents::LaunchPreset {
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: Some(Path::new("/abs/prompt.md").to_path_buf()),
            append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
        })
        .expect("claude renders model, effort, and prompt files natively");
    assert_eq!(
        argv,
        vec![
            "--model",
            "opus",
            "--effort",
            "high",
            "--system-prompt-file",
            "/abs/prompt.md",
            "--append-system-prompt-file",
            "/abs/append.md",
        ]
    );

    assert_eq!(
        ClaudeAdapter.max_turns_args(3),
        Some(vec!["--max-turns".to_owned(), "3".to_owned()])
    );

    // Empty preset renders nothing.
    assert!(
        ClaudeAdapter
            .render_preset(&crate::agents::LaunchPreset::default())
            .expect("empty preset is valid")
            .is_empty()
    );
}
