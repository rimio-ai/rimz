use serde_json::json;

use super::*;
use crate::feed::ResolutionMethod;
use crate::run::PermissionMode;
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
fn codex_commands_and_permission_args_match_run_posture() {
    let argv = CodexAdapter
        .resume_command("sess-abc", Path::new("/code/query-engine"))
        .expect("codex resumes");
    assert_eq!(argv, vec!["codex", "resume", "sess-abc"]);

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

    assert_eq!(
        CodexAdapter.permission_args(PermissionMode::Auto),
        vec![
            "--ask-for-approval",
            "never",
            "--sandbox",
            "workspace-write"
        ]
    );
    assert!(CodexAdapter.permission_args(PermissionMode::Ask).is_empty());
    assert_eq!(
        CodexAdapter.permission_args(PermissionMode::Yolo),
        vec!["--dangerously-bypass-approvals-and-sandbox"]
    );
}

#[test]
fn codex_render_preset_maps_effort_and_system_prompt_file_to_config_overrides() {
    let argv = CodexAdapter
        .render_preset(&crate::agents::LaunchPreset {
            model: Some("gpt-5-codex".to_owned()),
            effort: Some("high".to_owned()),
            system_prompt_file: Some(Path::new("/abs/prompt.md").to_path_buf()),
            ..Default::default()
        })
        .expect("codex renders model, effort, and instructions file via -c overrides");
    assert_eq!(
        argv,
        vec![
            "--model",
            "gpt-5-codex",
            "-c",
            "model_reasoning_effort=high",
            "-c",
            "model_instructions_file=/abs/prompt.md",
        ]
    );

    let err = CodexAdapter
        .render_preset(&crate::agents::LaunchPreset {
            append_system_prompt_file: Some(Path::new("/abs/append.md").to_path_buf()),
            ..Default::default()
        })
        .expect_err("codex has no append prompt flag");
    assert_eq!(
        err,
        crate::agents::PresetErr::UnsupportedField {
            agent: "codex",
            field: "append-system-prompt-file",
        }
    );
}

#[test]
fn codex_descriptor_declares_lazy_registration_and_idle_card_fallbacks() {
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
    assert_eq!(CodexAdapter.descriptor().default_model, Some("GPT-5.5"));
    assert_eq!(
        CodexAdapter.descriptor().default_context_window,
        Some(272_000)
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
