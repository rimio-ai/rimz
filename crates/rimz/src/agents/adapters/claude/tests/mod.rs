use super::*;
use crate::agents::TurnErrorClass;
use crate::agents::{HookIngressAcceptance, HookIngressDecision, HookIngressIgnoreReason};
use crate::harness::run::PermissionMode;
use serde_json::json;
use std::path::Path;

mod context;
mod install;
mod install_statusline;
mod lifecycle;
mod subagents;

#[test]
fn hook_ingress_ignores_remote_control_and_preserves_ordinary_owner() {
    assert_eq!(
        hook_ingress_decision(Some(42), true),
        HookIngressDecision::Ignore(HookIngressIgnoreReason::ClaudeRemoteControl)
    );
    assert_eq!(
        hook_ingress_decision(Some(42), false),
        HookIngressDecision::Accept(HookIngressAcceptance::agent(Some(42)))
    );
}

#[test]
fn claude_commands_and_permission_args_match_run_posture() {
    let argv = ClaudeAdapter
        .resume_command("sess-123", Path::new("/code/query-engine"))
        .expect("claude resumes");
    assert_eq!(argv, vec!["claude", "--resume", "sess-123"]);

    assert_eq!(
        ClaudeAdapter.spec().launch.fork_command("sess-123"),
        Some(
            ["claude", "--resume", "sess-123", "--fork-session"]
                .map(ToOwned::to_owned)
                .to_vec()
        )
    );

    assert_eq!(
        ClaudeAdapter.launch_command(&[], None),
        Some(vec!["claude".to_owned()])
    );
    assert_eq!(
        ClaudeAdapter.launch_command(&[], Some("review this")),
        Some(vec![
            "claude".to_owned(),
            "--".to_owned(),
            "review this".to_owned()
        ])
    );
    assert_eq!(
        ClaudeAdapter.launch_command(&[], Some("")),
        Some(vec!["claude".to_owned()])
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
            "--".to_owned(),
            "review this".to_owned()
        ])
    );

    assert_eq!(
        ClaudeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        vec!["--permission-mode", "auto"]
    );
    assert!(
        ClaudeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask)
            .is_empty()
    );
    assert_eq!(
        ClaudeAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        vec!["--dangerously-skip-permissions"]
    );
    assert_eq!(
        ClaudeAdapter.spec().launch.compact_command(),
        Some("/compact")
    );
    assert_eq!(
        ClaudeAdapter.spec().launch.max_turns_args(3),
        Some(vec!["--max-turns".to_owned(), "3".to_owned()])
    );
}
