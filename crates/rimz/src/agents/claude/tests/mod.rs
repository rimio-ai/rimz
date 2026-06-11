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
        vec!["--permission-mode", "acceptEdits"]
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
