use super::*;
use crate::agents::{AgentHookClass, TurnErrorClass};
use crate::feed::ResolutionMethod;
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
fn resume_command_is_claude_resume_with_the_session_id() {
    let argv = ClaudeAdapter
        .resume_command("sess-123", Path::new("/code/query-engine"))
        .expect("claude resumes");
    assert_eq!(argv, vec!["claude", "--resume", "sess-123"]);
}

#[test]
fn launch_command_is_claude_with_optional_prompt() {
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
}
