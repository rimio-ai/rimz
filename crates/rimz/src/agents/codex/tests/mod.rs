use serde_json::json;

use super::*;
use crate::agents::AgentHookClass;
use crate::feed::ResolutionMethod;
use std::io::Write;
use std::path::Path;

mod decision;
mod install;
mod lifecycle;
mod transcript;

fn fixture(kind: FeedKind) -> FeedItem {
    crate::agents::testkit::feed_item(kind, "codex")
}

#[test]
fn resume_command_is_codex_resume_with_the_session_id() {
    let argv = CodexAdapter
        .resume_command("sess-abc", Path::new("/code/query-engine"))
        .expect("codex resumes");
    assert_eq!(argv, vec!["codex", "resume", "sess-abc"]);
}

#[test]
fn launch_command_is_codex_with_optional_prompt() {
    assert_eq!(
        CodexAdapter.launch_command(&[], None),
        Some(vec!["codex".to_owned()])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("review this")),
        Some(vec!["codex".to_owned(), "review this".to_owned()])
    );
    assert_eq!(
        CodexAdapter.launch_command(
            &[
                "--model".to_owned(),
                "gpt-5-codex".to_owned(),
                "-c".to_owned(),
                "model_reasoning_effort=high".to_owned()
            ],
            Some("review this")
        ),
        Some(vec![
            "codex".to_owned(),
            "--model".to_owned(),
            "gpt-5-codex".to_owned(),
            "-c".to_owned(),
            "model_reasoning_effort=high".to_owned(),
            "review this".to_owned()
        ])
    );
}

#[test]
fn codex_registers_its_session_lazily() {
    // Codex's instances can be present before a session binds (lazy
    // `SessionStart`, daemon-routed unstamped hooks), so it opts into the
    // sidebar's cwd-bind + idle-instance synthesis. Claude declares the
    // opposite (it stamps a pane on every session).
    assert!(CodexAdapter.descriptor().capabilities.registers_lazily);
    assert!(
        !crate::agents::ClaudeAdapter
            .descriptor()
            .capabilities
            .registers_lazily
    );
}

#[test]
fn codex_declares_idle_card_fallbacks() {
    assert_eq!(CodexAdapter.descriptor().default_model, Some("GPT-5.5"));
    assert_eq!(
        CodexAdapter.descriptor().default_context_window,
        Some(258_000)
    );
}

#[test]
fn configured_model_reads_codex_launch_default() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
model = "o4-mini"
model_reasoning_effort = "high"
"#,
    )
    .unwrap();

    assert_eq!(configured_model_at(&path).as_deref(), Some("o4-mini"));
}

#[test]
fn configured_reasoning_effort_reads_the_actual_codex_setting() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
model = "gpt-5.5-codex"
model_reasoning_effort = "xhigh"
plan_mode_reasoning_effort = "medium"
"#,
    )
    .unwrap();

    assert_eq!(
        configured_reasoning_effort_at(&path).as_deref(),
        Some("xhigh")
    );
}
