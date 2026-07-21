use serde_json::json;

use super::*;
use crate::agents::testkit::hook_output;
use crate::agents::{
    HookIngressAcceptance, HookIngressDecision, HookIngressIgnoreReason, HookIngressOwner,
};
use crate::harness::run::PermissionMode;
use std::io::Write;
use std::path::Path;

mod ask;
mod install;
mod lifecycle;
mod transcript;

#[test]
fn hook_ingress_ignores_internal_servers_and_normalizes_daemon_owners() {
    assert_eq!(
        hook_ingress_decision(Some(42), true, false),
        HookIngressDecision::Ignore(HookIngressIgnoreReason::CodexInternalAppServer)
    );
    assert_eq!(
        hook_ingress_decision(Some(42), false, true),
        HookIngressDecision::Accept(HookIngressAcceptance {
            owner: HookIngressOwner {
                pid: Some(42),
                kind: crate::pane::RuntimeOwnerKind::Daemon,
            },
            participant_start: None,
        })
    );
    assert_eq!(
        hook_ingress_decision(Some(42), false, false),
        HookIngressDecision::Accept(HookIngressAcceptance::agent(Some(42)))
    );
}

#[test]
fn codex_commands_and_permission_args_match_run_posture() {
    let argv = CodexAdapter
        .resume_command("sess-abc", Path::new("/code/query-engine"))
        .expect("codex resumes");
    assert_eq!(argv, vec!["codex", "resume", "sess-abc"]);

    assert_eq!(
        CodexAdapter.spec().launch.fork_command("sess-abc"),
        Some(
            ["codex", "fork", "sess-abc"]
                .map(ToOwned::to_owned)
                .to_vec()
        )
    );

    assert_eq!(
        CodexAdapter.launch_command(&[], None),
        Some(vec!["codex".to_owned()])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("review this")),
        Some(vec![
            "codex".to_owned(),
            "--".to_owned(),
            "review this".to_owned()
        ])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("")),
        Some(vec!["codex".to_owned()])
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
            "--".to_owned(),
            "review this".to_owned()
        ])
    );

    assert_eq!(
        CodexAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Auto),
        vec![
            "--ask-for-approval",
            "never",
            "--sandbox",
            "workspace-write"
        ]
    );
    assert!(
        CodexAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask)
            .is_empty()
    );
    assert_eq!(
        CodexAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Yolo),
        vec!["--dangerously-bypass-approvals-and-sandbox"]
    );
    assert_eq!(
        CodexAdapter.spec().launch.compact_command(),
        Some("/compact")
    );
}

#[test]
fn codex_descriptor_declares_lazy_registration() {
    // Codex's instances can be present before a session binds (lazy
    // `SessionStart`, daemon-routed unstamped hooks), so it opts into cwd
    // session binding. The idle-synthesis observation gate is separate; Claude
    // stays out of cwd binding because it stamps every session.
    assert!(CodexAdapter.spec().capabilities.registers_lazily);
    assert!(
        !crate::agents::definition_by_kind("claude")
            .expect("Claude definition")
            .spec()
            .capabilities
            .registers_lazily
    );
    assert_eq!(CodexAdapter.spec().default_model, Some("gpt-5.5-codex"));
    assert_eq!(CodexAdapter.spec().default_context_window, Some(272_000));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    let launch_model = with_codex_config_path(&path, || CodexAdapter.default_launch_model());
    assert_eq!(launch_model.as_deref(), Some("gpt-5.5-codex"));
}

#[test]
fn codex_question_summary_reads_request_user_input_questions() {
    let questions = hook_output(
        &CodexAdapter,
        "PreToolUse",
        &json!({
            "tool_name": "request_user_input",
            "tool_input": {
                "questions": [
                    { "question": "Pick a migration path?" },
                    { "question": "Notify users?" }
                ]
            }
        }),
    )
    .questions()
    .to_vec();

    assert_eq!(
        questions,
        vec![
            crate::transcript::AskQuestion {
                question: "Pick a migration path?".to_owned(),
                options: Vec::new(),
                multi_select: false,
                has_option_previews: false,
            },
            crate::transcript::AskQuestion {
                question: "Notify users?".to_owned(),
                options: Vec::new(),
                multi_select: false,
                has_option_previews: false,
            },
        ]
    );
    assert!(
        hook_output(
            &CodexAdapter,
            "PreToolUse",
            &json!({
                "tool_name": "shell",
                "tool_input": { "questions": [{ "question": "ignored" }] }
            })
        )
        .questions()
        .to_vec()
        .is_empty()
    );
}

#[test]
fn configured_model_and_reasoning_effort_read_codex_config() {
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

    assert_eq!(configured_model_at(&path).as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(
        configured_reasoning_effort_at(&path).as_deref(),
        Some("xhigh")
    );
}

#[test]
fn configured_identity_reads_codex_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        r#"
model = "gpt-5.5-codex"
model_reasoning_effort = "xhigh"
"#,
    )
    .unwrap();

    let (model, effort) = with_codex_config_path(&path, || CodexAdapter.configured_identity());

    assert_eq!(model.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(effort.as_deref(), Some("xhigh"));
}
