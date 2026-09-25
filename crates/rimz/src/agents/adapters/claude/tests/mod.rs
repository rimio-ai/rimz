use super::*;

use crate::agents::PermissionMode;
use crate::agents::TurnErrorClass;
use crate::agents::{HookIngressAcceptance, HookIngressDecision, HookIngressIgnoreReason};
use serde_json::json;
use std::path::Path;

mod context;
mod install;
mod install_statusline;
mod lifecycle;
mod local_context;
mod subagents;

#[test]
fn host_skills_merge_settings_once_and_use_directory_names() {
    use crate::agents::skills::{HostSkills, SkillDir};
    let root = tempfile::tempdir().unwrap();
    let settings = root.path().join("settings.json");
    std::fs::write(
        &settings,
        r#"{ // retain other settings
      "env":{"ANTHROPIC_API_KEY":"sk-secret-123"}, "theme":"dark", "skillOverrides":{"listed":"enabled","unlisted":"enabled"},
    }"#,
    )
    .unwrap();
    let HostSkills::Switch { key, render, .. } = CLAUDE_DESCRIPTOR.host_skills else {
        panic!("missing switch")
    };
    let skill = SkillDir {
        name: "unlisted".into(),
        source: root.path().to_owned(),
    };
    std::fs::write(root.path().join("SKILL.md"), "---\nname: different\n---\n").unwrap();
    let key = key(&skill).unwrap();
    assert_eq!(key.as_str(), "unlisted");
    let mut args = vec![
        "--settings={\"ignored\":true}".to_owned(),
        "--settings".to_owned(),
        "settings.json".into(),
    ];
    let artifact = render(&[key], root.path(), root.path(), &mut args)
        .unwrap()
        .unwrap();
    let (artifact_path, merged) = &artifact;
    assert!(!args.join(" ").contains("sk-secret-123"));
    assert_eq!(args, ["--settings", artifact_path.to_str().unwrap()]);
    assert!(!artifact_path.exists());
    assert_eq!(
        merged,
        &json!({
            "env": {"ANTHROPIC_API_KEY": "sk-secret-123"},
            "theme": "dark",
            "skillOverrides": {"listed": "enabled", "unlisted": "user-invocable-only"}
        })
    );
    let mut inline = vec!["--settings".into(), merged.to_string()];
    let inline_artifact = render(&[], root.path(), root.path(), &mut inline).unwrap();
    assert_eq!(inline_artifact, Some(artifact));
    assert_eq!(inline, args);
    let mut bare = Vec::new();
    assert!(
        render(&[], root.path(), root.path(), &mut bare)
            .unwrap()
            .is_none()
    );
    assert_eq!(bare, ["--settings", r#"{"skillOverrides":{}}"#]);
    let odd = root.path().join("odd name");
    std::fs::create_dir(&odd).unwrap();
    std::fs::write(odd.join("SKILL.md"), "skill").unwrap();
    let (plan, _) = CLAUDE_DESCRIPTOR
        .host_skills
        .apply(
            &[],
            Some(root.path()),
            None,
            (root.path(), root.path()),
            &mut Vec::new(),
        )
        .unwrap();
    let crate::agents::skills::HostSkillPlan::Applied { unlisted, .. } = plan else {
        panic!("missing host skill plan")
    };
    assert_eq!(
        unlisted.iter().map(|key| key.as_str()).collect::<Vec<_>>(),
        ["odd name"]
    );
    let mut invalid = vec![
        "--settings".into(),
        root.path().join("missing.json").display().to_string(),
    ];
    assert!(
        render(&[], root.path(), root.path(), &mut invalid)
            .unwrap_err()
            .to_string()
            .contains("missing.json")
    );
}

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
fn named_login_scopes_spending_and_session_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let named = tmp.path().join("work");
    let native_file = home
        .join(".claude")
        .join("projects/project/shared-session.jsonl");
    let named_file = named.join("projects/project/shared-session.jsonl");
    for path in [&native_file, &named_file] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }
    let home_env = std::collections::BTreeMap::from([(
        "HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )]);
    let named_env = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("claude"),
        "work".parse().unwrap(),
        named,
    )
    .unwrap()
    .env(&home_env);
    let files = ClaudeAdapter
        .spending_sources(&named_env)
        .into_iter()
        .flat_map(|source| source.complete_files())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![named_file.clone()]);
    assert_eq!(
        ClaudeAdapter.session_transcript("shared-session", None, &named_env),
        Some(named_file),
    );
}

#[test]
fn claude_commands_and_permission_args_match_run_posture() {
    let preset = crate::agents::LaunchPreset {
        auto_compact: Some("200000".to_owned()),
        ..Default::default()
    };
    assert!(!preset.is_empty());
    assert_eq!(
        ClaudeAdapter.spec().render_preset(&preset).unwrap(),
        vec!["--autocompact", "200000"]
    );
    assert_eq!(
        ClaudeAdapter
            .spec()
            .launch
            .preset_arg_matcher(crate::agents::PresetField::AutoCompact),
        Some(crate::agents::PresetArgMatcher::Flag(vec![
            "--autocompact".to_owned()
        ]))
    );

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
        ClaudeAdapter.spec().launch.compact_command(""),
        Some("/compact".to_owned())
    );
    assert_eq!(
        ClaudeAdapter
            .spec()
            .launch
            .compact_command("preserve the implementation plan"),
        Some("/compact preserve the implementation plan".to_owned())
    );
    assert_eq!(
        ClaudeAdapter.spec().launch.max_turns_args(3),
        Some(vec!["--max-turns".to_owned(), "3".to_owned()])
    );
}

#[test]
fn subagent_lockdown_merges_claude_disallowed_tools_once() {
    let mut args = [
        "--disallowedTools",
        "Agent(fork)",
        "Bash",
        "--tools",
        "Agent,Bash",
        "--disallowed-tools=Agent(statusline-setup)",
        "--disallowed-tools",
        "Write",
        "Bash",
        "--model",
        "opus",
    ]
    .map(ToOwned::to_owned)
    .to_vec();

    ClaudeAdapter.lockdown_subagent_args(&mut args);

    assert_eq!(
        args,
        [
            "--tools",
            "Agent,Bash",
            "--model",
            "opus",
            "--disallowedTools",
            "Bash",
            "Write",
            "Agent",
        ]
        .map(ToOwned::to_owned)
    );
}

#[test]
fn subagent_lockdown_handles_claude_equals_and_empty_forms() {
    let mut equals = vec!["--disallowedTools=Read".to_owned()];
    ClaudeAdapter.lockdown_subagent_args(&mut equals);
    assert_eq!(equals, vec!["--disallowedTools", "Read", "Agent"]);

    let mut empty = vec!["--model".to_owned(), "opus".to_owned()];
    ClaudeAdapter.lockdown_subagent_args(&mut empty);
    assert_eq!(empty, vec!["--model", "opus", "--disallowedTools", "Agent"]);
}

#[test]
fn shared_lsp_denial_composes_with_child_lockdown_in_either_order() {
    for child_first in [false, true] {
        let mut args = [
            "--disallowedTools=Read",
            "--disallowed-tools",
            "Write",
            "--tools",
            "Bash,LSP",
        ]
        .map(str::to_owned)
        .to_vec();
        if child_first {
            ClaudeAdapter.lockdown_subagent_args(&mut args);
        }
        ClaudeAdapter.disable_native_lsp_args(&mut args);
        if !child_first {
            assert!(!args.iter().any(|arg| arg == "Agent"));
            ClaudeAdapter.lockdown_subagent_args(&mut args);
        }
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--disallowedTools")
                .count(),
            1
        );
        assert_eq!(args.iter().filter(|arg| *arg == "LSP").count(), 1);
        assert!(args.iter().any(|arg| arg == "Agent"));
        assert!(args.iter().any(|arg| arg == "Read"));
        assert!(args.iter().any(|arg| arg == "Write"));
    }
}
