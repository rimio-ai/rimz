use serde_json::json;

use super::transcript::{
    configured_model_at, configured_reasoning_effort_at, with_codex_config_path,
};
use super::*;

use crate::agents::PermissionMode;
use crate::agents::testkit::hook_output;
use crate::agents::{
    HookIngressAcceptance, HookIngressDecision, HookIngressIgnoreReason, HookIngressOwner,
};
use std::io::Write;
use std::path::Path;

mod ask;
mod install;
mod lifecycle;
mod project_trust;
mod transcript;

#[test]
fn headless_result_reads_verdict_file_and_prices_last_completed_turn() {
    let root = tempfile::tempdir().unwrap();
    let request = crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: root.path().join("schema.json"),
        verdict_file: root.path().join("verdict.json"),
    };
    std::fs::write(
        &request.verdict_file,
        r#"{"pass":true,"reason":"Work remains."}"#,
    )
    .unwrap();
    let prices = crate::agents::pricing::PriceBook::from_litellm_json(
        r#"{"check-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.01,"cache_read_input_token_cost":0.0001}}"#,
    );
    let stdout = br#"{"type":"thread.started","thread_id":"thread"}
{"type":"turn.completed","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1}}
{"type":"item.completed","item":{"type":"agent_message","text":"not the verdict"}}
{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":40,"output_tokens":5}}
"#;
    let result = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"",
        &request,
        Some("check-model"),
        &prices,
    );
    assert_eq!(
        result.verdict,
        Some(crate::agents::HeadlessVerdict {
            pass: true,
            reason: "Work remains.".into()
        })
    );
    assert_eq!(result.input_tokens, Some(100));
    assert_eq!(result.output_tokens, Some(5));
    assert!((result.cost_usd.unwrap() - 0.114).abs() < 1e-12);
    assert_eq!(result.error, None);
    let noisy_stdout = [
        b"shell startup message\n".as_slice(),
        stdout,
        b"shell diagnostic\n\n",
    ]
    .concat();
    let noisy = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        &noisy_stdout,
        b"",
        &request,
        Some("check-model"),
        &prices,
    );
    assert_eq!(
        noisy, result,
        "startup output must not hide the verdict or usage"
    );
    let unpriced = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"",
        &request,
        Some("unknown-check-model"),
        &prices,
    );
    assert_eq!(unpriced.cost_usd, None);
    assert_eq!(unpriced.verdict, result.verdict);
    assert_eq!(unpriced.input_tokens, Some(100));
    let prices = crate::agents::pricing::PriceBook::from_litellm_json(
        r#"{"check-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.01}}"#,
    );
    let implicit = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"",
        &request,
        Some("check-model"),
        &prices,
    );
    assert!((implicit.cost_usd.unwrap() - 0.15).abs() < 1e-12);
}

#[test]
fn headless_result_rejects_missing_and_malformed_verdict_files() {
    let root = tempfile::tempdir().unwrap();
    let request = crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: root.path().join("schema.json"),
        verdict_file: root.path().join("verdict.json"),
    };
    let form = CODEX_DESCRIPTOR.launch.headless.unwrap();
    let prices = crate::agents::pricing::PriceBook::fixture();
    let result = form.read_result(b"", b"", &request, None, &prices);
    assert!(result.error.is_some());
    for verdict in [
        "garbage",
        r#"{"pass":false}"#,
        r#"{"pass":true,"reason":"bad","extra":1}"#,
    ] {
        std::fs::write(&request.verdict_file, verdict).unwrap();
        let result = form.read_result(b"", b"", &request, None, &prices);
        assert!(result.error.is_some());
        assert_eq!(result.verdict, None);
    }
    std::fs::remove_file(&request.verdict_file).unwrap();
    let result = form.read_result(b"not JSON", b"", &request, None, &prices);
    assert!(result.error.is_some());
    assert_eq!(result.verdict, None);
}

#[test]
fn deadline_context_reply_matches_native_post_tool_contract() {
    let (post, other) = crate::agents::testkit::deadline_context_replies("codex", "PostToolUse");
    insta::assert_json_snapshot!(post, @r#"
    {
      "hookSpecificOutput": {
        "additionalContext": "deadline context",
        "hookEventName": "PostToolUse"
      }
    }
    "#);
    insta::assert_json_snapshot!(other, @"null");
}

#[test]
fn prompt_context_reply_matches_native_prompt_submit_contract() {
    insta::assert_snapshot!(
        crate::agents::testkit::prompt_context_stdout("codex", "UserPromptSubmit"),
        @r#"{"hookSpecificOutput":{"additionalContext":"prompt context","hookEventName":"UserPromptSubmit"}}"#
    );
}

#[test]
fn host_skills_replace_cli_config_and_use_frontmatter_names() {
    use crate::agents::skills::{HostSkills, SkillDir};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("SKILL.md"),
        "---\nname: 'provider-name'\n---\n",
    )
    .unwrap();
    let HostSkills::Switch { key, render, .. } = CODEX_DESCRIPTOR.host_skills else {
        panic!("missing switch")
    };
    let key = key(&SkillDir {
        name: "directory".into(),
        source: root.path().to_owned(),
    })
    .unwrap();
    assert_eq!(key.as_str(), "provider-name");
    let broken = root.path().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("SKILL.md"), "---\nname: [unclosed\n---\n").unwrap();
    let fallback = host_skill_key(&SkillDir {
        name: "broken".into(),
        source: broken,
    })
    .unwrap();
    assert_eq!(fallback.as_str(), "broken");
    let mut args = vec![
        "--config=skills.config=[]".into(),
        "-c".into(),
        "skills.config=[]".into(),
        "-c".into(),
        "keep=true".into(),
    ];
    render(&[key], root.path(), root.path(), &mut args).unwrap();
    insta::assert_json_snapshot!(args, @r#"
    [
      "-c",
      "keep=true",
      "-c",
      "skills.config=[{name=\"provider-name\",enabled=false}]"
    ]
    "#);
}

#[test]
fn host_skills_refuse_conflicting_provider_keys() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["listed", "unlisted"] {
        let path = root.path().join(directory);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("SKILL.md"), "---\nname: shared-name\n---\n").unwrap();
    }
    let result = CODEX_DESCRIPTOR.host_skills.apply(
        &["listed".parse().unwrap()],
        Some(root.path()),
        None,
        (root.path(), root.path()),
        &mut Vec::new(),
    );
    assert!(
        result.is_err(),
        "conflicting provider keys must refuse: {result:?}"
    );
}

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
fn config_home_prefers_codex_home_then_falls_back_to_home_dot_codex() {
    let env = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    assert_eq!(
        CodexAdapter.config_home(&env(&[("CODEX_HOME", "/srv/codex"), ("HOME", "/home/u")])),
        Some(PathBuf::from("/srv/codex"))
    );
    assert_eq!(
        CodexAdapter.config_home(&env(&[("CODEX_HOME", ""), ("HOME", "/home/u")])),
        Some(PathBuf::from("/home/u/.codex"))
    );
    assert_eq!(CodexAdapter.config_home(&env(&[])), None);
}

#[test]
fn named_login_scopes_spending_and_session_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let named = tmp.path().join("work");
    let native_file = home
        .join(".codex")
        .join("sessions/2026/01/01/rollout-2026-01-01T00-00-00-shared-session.jsonl");
    let named_file =
        named.join("sessions/2026/01/01/rollout-2026-01-01T00-00-00-shared-session.jsonl");
    for path in [&native_file, &named_file] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }
    let home_env = std::collections::BTreeMap::from([(
        "HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )]);
    let named_env = crate::agents::ProviderLogin::named(
        crate::ids::AgentKind::new_unchecked("codex"),
        "work".parse().unwrap(),
        named,
    )
    .unwrap()
    .env(&home_env);
    let files = CodexAdapter
        .spending_sources(&named_env)
        .into_iter()
        .flat_map(|source| source.complete_files())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![named_file.clone()]);
    assert_eq!(
        CodexAdapter.session_transcript("shared-session", None, &named_env),
        Some(named_file),
    );
}

#[test]
fn spending_discovery_skips_copied_content_and_keeps_relocated_rollouts() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let active = home.join("sessions/2026/01/01/rollout-2026-01-01T00-00-00-a.jsonl");
    let relocated = home.join("rollout.jsonl");
    let copied = [
        ".tmp/plugins/plugins/plugin-eval/fixtures/observed-usage/responses.jsonl",
        "plugins/cache/market/plugin/1.0.0/fixtures/usage.jsonl",
        "skills/cloned-skill/fixtures/usage.jsonl",
        "worktrees/repo/fixtures/usage.jsonl",
        "packages/standalone/current/usage.jsonl",
        "cache/codex_app_directory/usage.jsonl",
        "tmp/arg0/usage.jsonl",
        "log/session-2026-01-01T00-00-00Z.jsonl",
    ];
    for path in [active.clone(), relocated.clone()]
        .into_iter()
        .chain(copied.iter().map(|relative| home.join(relative)))
    {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "{}\n").unwrap();
    }
    let env = std::collections::BTreeMap::from([(
        "CODEX_HOME".to_owned(),
        home.to_string_lossy().into_owned(),
    )]);
    let files = CodexAdapter
        .spending_sources(&env)
        .into_iter()
        .flat_map(|source| source.complete_files())
        .collect::<Vec<_>>();
    assert_eq!(files, vec![active, relocated]);
}

#[test]
fn codex_commands_and_permission_args_match_run_posture() {
    let preset = crate::agents::LaunchPreset {
        auto_compact: Some("200000".to_owned()),
        ..Default::default()
    };
    assert!(!preset.is_empty());
    assert_eq!(
        CodexAdapter.spec().render_preset(&preset).unwrap(),
        vec!["-c", "model_auto_compact_token_limit=200000"]
    );
    assert_eq!(
        CodexAdapter
            .spec()
            .launch
            .preset_arg_matcher(crate::agents::PresetField::AutoCompact),
        Some(crate::agents::PresetArgMatcher::ConfigKey {
            flags: vec!["-c".to_owned(), "--config".to_owned()],
            key: "model_auto_compact_token_limit".to_owned(),
        })
    );

    let argv = CodexAdapter
        .resume_command("sess-abc", Path::new("/code/query-engine"))
        .expect("codex resumes");
    assert_eq!(argv, vec!["codex", "resume", "sess-abc", "--no-daemon"]);

    assert_eq!(
        CodexAdapter.spec().launch.fork_command("sess-abc"),
        Some(
            ["codex", "fork", "sess-abc", "--no-daemon"]
                .map(ToOwned::to_owned)
                .to_vec()
        )
    );

    assert_eq!(
        CodexAdapter.launch_command(&[], None),
        Some(vec!["codex".to_owned(), "--no-daemon".to_owned()])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("review this")),
        Some(vec![
            "codex".to_owned(),
            "--no-daemon".to_owned(),
            "--".to_owned(),
            "review this".to_owned()
        ])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("")),
        Some(vec!["codex".to_owned(), "--no-daemon".to_owned()])
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
            "--no-daemon".to_owned(),
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
        CodexAdapter
            .spec()
            .launch
            .compact_command("preserve the implementation plan"),
        Some("/compact".to_owned())
    );
}

#[test]
fn subagent_lockdown_replaces_codex_multi_agent_overrides() {
    let mut args = [
        "-c",
        "features.multi_agent=true",
        "--config",
        "model_reasoning_effort=high",
        "--config",
        "features.multi_agent=true",
        "--model",
        "gpt-5",
    ]
    .map(ToOwned::to_owned)
    .to_vec();

    CodexAdapter.lockdown_subagent_args(&mut args);

    assert_eq!(
        args,
        [
            "--config",
            "model_reasoning_effort=high",
            "--model",
            "gpt-5",
            "-c",
            "features.multi_agent=false",
        ]
        .map(ToOwned::to_owned)
    );
}

#[test]
fn sandbox_isolation_replaces_codex_sandbox_overrides() {
    let mut args = [
        "--sandbox",
        "workspace-write",
        "-s",
        "read-only",
        "--sandbox=workspace-write",
        "-c",
        "sandbox_mode=read-only",
        "--config=sandbox_mode=workspace-write",
        "--ask-for-approval",
        "never",
    ]
    .map(ToOwned::to_owned)
    .to_vec();

    CodexAdapter.disable_native_sandbox_args(&mut args);
    let once = args.clone();
    CodexAdapter.disable_native_sandbox_args(&mut args);

    assert_eq!(
        args,
        [
            "--ask-for-approval",
            "never",
            "--sandbox",
            "danger-full-access"
        ]
        .map(ToOwned::to_owned)
    );
    assert_eq!(args, once);

    let mut approve_for_me = vec!["--not-so-yolo".to_owned(), "--approve-for-me".to_owned()];
    CodexAdapter.disable_native_sandbox_args(&mut approve_for_me);
    assert_eq!(
        approve_for_me,
        [
            "-c",
            r#"approvals_reviewer="auto_review""#,
            "-c",
            r#"approval_policy="on-request""#,
            "--sandbox",
            "danger-full-access",
        ]
    );

    let mut yolo = vec!["--dangerously-bypass-approvals-and-sandbox".to_owned()];
    CodexAdapter.disable_native_sandbox_args(&mut yolo);
    assert_eq!(yolo, ["--dangerously-bypass-approvals-and-sandbox"]);
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
    assert_eq!(CodexAdapter.spec().default_model, None);
    assert_eq!(CodexAdapter.spec().default_context_window, Some(272_000));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "").unwrap();
    let launch_model = with_codex_config_path(&path, || CodexAdapter.default_launch_model());
    assert_eq!(launch_model, None);

    std::fs::write(&path, "model = \"gpt-6-astra\"\n").unwrap();
    let launch_model = with_codex_config_path(&path, || CodexAdapter.default_launch_model());
    assert_eq!(launch_model.as_deref(), Some("gpt-6-astra"));
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
