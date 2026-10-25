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
fn headless_schema_enforcement_matches_the_schema_contract() {
    let root = tempfile::tempdir().unwrap();
    let form = CODEX_DESCRIPTOR.launch.headless.unwrap();
    for schema_open in [false, true] {
        let request = crate::agents::HeadlessRequest {
            schema: "{}".into(),
            schema_open,
            schema_file: root.path().join("schema.json"),
            verdict_file: root.path().join("verdict.json"),
        };
        let argv = form
            .render_argv(
                &[],
                Some("Return one JSON object"),
                &request,
                root.path(),
                (root.path(), &mut None),
            )
            .unwrap();
        assert_eq!(
            argv.iter().any(|arg| arg == "--output-schema"),
            !schema_open,
            "Codex strict output cannot accept an open schema"
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair == ["-o", request.verdict_file.to_str().unwrap()])
        );
    }
}

#[test]
fn headless_result_reads_verdict_file_and_prices_last_completed_turn() {
    let root = tempfile::tempdir().unwrap();
    let request = crate::agents::HeadlessRequest {
        schema_open: false,
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
        false,
        &prices,
    );
    assert_eq!(
        result.verdict,
        Some(json!({"pass":true,"reason":"Work remains."}))
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
        false,
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
        false,
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
        false,
        &prices,
    );
    assert!((implicit.cost_usd.unwrap() - 0.15).abs() < 1e-12);
    let prices = crate::agents::pricing::PriceBook::from_litellm_json(
        r#"{"check-model":{"input_cost_per_token":0.001,"output_cost_per_token":0.01,"cache_read_input_token_cost":0.0001,"provider_specific_entry":{"fast":2.5}}}"#,
    );
    let fast = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"",
        &request,
        Some("check-model"),
        true,
        &prices,
    );
    assert!(
        (fast.cost_usd.unwrap() - 0.114 * 2.5).abs() < 1e-12,
        "a check launched in fast mode prices with the fast multiplier"
    );
    let standard = CODEX_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"",
        &request,
        Some("check-model"),
        false,
        &prices,
    );
    assert!((standard.cost_usd.unwrap() - 0.114).abs() < 1e-12);
}

#[test]
fn headless_result_rejects_missing_and_malformed_verdict_files() {
    let root = tempfile::tempdir().unwrap();
    let request = crate::agents::HeadlessRequest {
        schema_open: false,
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: root.path().join("schema.json"),
        verdict_file: root.path().join("verdict.json"),
    };
    let form = CODEX_DESCRIPTOR.launch.headless.unwrap();
    let prices = crate::agents::pricing::PriceBook::fixture();
    let result = form.read_result(b"", b"", &request, None, false, &prices);
    assert!(result.error.is_some());
    for verdict in [
        "garbage",
        r#"{"pass":false}"#,
        r#"{"pass":true,"reason":"bad","extra":1}"#,
    ] {
        std::fs::write(&request.verdict_file, verdict).unwrap();
        let result = form.read_result(b"", b"", &request, None, false, &prices);
        assert!(
            result
                .verdict
                .and_then(
                    |value| serde_json::from_value::<crate::agents::HeadlessVerdict>(value).ok()
                )
                .is_none()
        );
    }
    std::fs::remove_file(&request.verdict_file).unwrap();
    let result = form.read_result(b"not JSON", b"", &request, None, false, &prices);
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
        (Some(&["listed".parse().unwrap()]), None),
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
    assert_eq!(
        CodexAdapter.spec().launch.picker,
        Some(
            [
                "codex",
                "resume",
                "--no-daemon",
                "-c",
                "tui.question_esc_back=false"
            ]
            .as_slice()
        )
    );
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
    assert_eq!(
        argv,
        vec![
            "codex",
            "resume",
            "sess-abc",
            "--no-daemon",
            "-c",
            "tui.question_esc_back=false"
        ]
    );

    assert_eq!(
        CodexAdapter.spec().launch.fork_command("sess-abc"),
        Some(
            [
                "codex",
                "fork",
                "sess-abc",
                "--no-daemon",
                "-c",
                "tui.question_esc_back=false"
            ]
            .map(ToOwned::to_owned)
            .to_vec()
        )
    );

    assert_eq!(
        CodexAdapter.launch_command(&[], None),
        Some(
            ["codex", "--no-daemon", "-c", "tui.question_esc_back=false"]
                .map(ToOwned::to_owned)
                .to_vec()
        )
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("review this")),
        Some(vec![
            "codex".to_owned(),
            "--no-daemon".to_owned(),
            "-c".to_owned(),
            "tui.question_esc_back=false".to_owned(),
            "--".to_owned(),
            "review this".to_owned()
        ])
    );
    assert_eq!(
        CodexAdapter.launch_command(&[], Some("")),
        Some(
            ["codex", "--no-daemon", "-c", "tui.question_esc_back=false"]
                .map(ToOwned::to_owned)
                .to_vec()
        )
    );
    assert_eq!(
        CodexAdapter.launch_command(&["-c".into(), "tui.question_esc_back=true".into()], None),
        Some(
            [
                "codex",
                "--no-daemon",
                "-c",
                "tui.question_esc_back=false",
                "-c",
                "tui.question_esc_back=true"
            ]
            .map(ToOwned::to_owned)
            .to_vec()
        )
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
            "-c".to_owned(),
            "tui.question_esc_back=false".to_owned(),
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
            "on-request",
            "-c",
            r#"approvals_reviewer="auto_review""#,
        ]
    );
    assert_eq!(
        CodexAdapter
            .spec()
            .launch
            .permission_args(PermissionMode::Ask),
        vec![
            "--ask-for-approval",
            "on-request",
            "-c",
            r#"approvals_reviewer="user""#,
        ]
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
fn host_room_grants_follow_the_users_permission_selection() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    let env = BTreeMap::from([("RIMZ_CODEX_CONFIG".into(), config.display().to_string())]);
    let roots = [
        PathBuf::from("/room/state space"),
        PathBuf::from("/run/room/quote\""),
        PathBuf::from("/run/shared\\root"),
    ];
    let legacy_grant = |user_roots: &[&str]| {
        let mut values = toml_edit::Array::new();
        for root in user_roots {
            values.push(*root);
        }
        for root in &roots {
            values.push(root.to_str().unwrap());
        }
        vec![
            "-c".to_owned(),
            format!("sandbox_workspace_write.writable_roots={values}"),
        ]
    };
    let profile_grant = |name: &str| {
        let mut values = toml_edit::InlineTable::new();
        for root in &roots {
            values.insert(root.to_str().unwrap(), true.into());
        }
        vec![
            "-c".to_owned(),
            format!("permissions.{name}.workspace_roots={values}"),
        ]
    };
    let workspace_grant = roots
        .iter()
        .flat_map(|root| ["--add-dir".to_owned(), root.display().to_string()])
        .collect::<Vec<_>>();
    let dev = "default_permissions = 'dev'\n[permissions.dev]\nextends = ':workspace'\nnetwork = { enabled = true }";
    let mut failures = Vec::new();
    for (content, overrides, expected) in [
        ("", vec![], legacy_grant(&[])),
        (
            "[sandbox_workspace_write]\nwritable_roots = ['/u']\nnetwork_access = true",
            vec![],
            legacy_grant(&["/u"]),
        ),
        (
            "sandbox_mode = 'workspace-write'",
            vec![],
            legacy_grant(&[]),
        ),
        ("sandbox_mode = 'read-only'", vec![], vec![]),
        ("sandbox_mode = 'danger-full-access'", vec![], vec![]),
        (
            "default_permissions = ':workspace'",
            vec![],
            workspace_grant,
        ),
        ("default_permissions = ':read-only'", vec![], vec![]),
        (
            "default_permissions = ':danger-full-access'",
            vec![],
            vec![],
        ),
        ("default_permissions = ':other'", vec![], vec![]),
        (dev, vec![], profile_grant("dev")),
        ("default_permissions = 'dev team'", vec![], vec![]),
        (
            "default_permissions = 'Dev_2-x'",
            vec![],
            profile_grant("Dev_2-x"),
        ),
        (dev, vec!["--sandbox", "danger-full-access"], vec![]),
        (dev, vec!["-s", "read-only"], vec![]),
        (dev, vec!["--sandbox=workspace-write"], legacy_grant(&[])),
        (
            dev,
            vec![
                "--dangerously-bypass-approvals-and-sandbox",
                "--sandbox",
                "workspace-write",
            ],
            vec![],
        ),
        (
            dev,
            vec!["-c", "sandbox_mode=\"workspace-write\""],
            legacy_grant(&[]),
        ),
        (dev, vec!["--approve-for-me"], legacy_grant(&[])),
        (dev, vec!["--not-so-yolo"], legacy_grant(&[])),
        (
            "",
            vec!["--approve-for-me", "-c", "default_permissions=\"dev\""],
            profile_grant("dev"),
        ),
        ("", vec!["--profile", "x"], legacy_grant(&[])),
        ("", vec!["-p=x"], legacy_grant(&[])),
        (dev, vec!["--profile=x"], profile_grant("dev")),
        (
            "",
            vec!["--profile", "x", "-c", "sandbox_mode=workspace-write"],
            legacy_grant(&[]),
        ),
        ("default_permissions = 'a.b'", vec![], vec![]),
        ("", vec!["--config=default_permissions=a.b"], vec![]),
        (
            "sandbox_mode = 'read-only'\ndefault_permissions = 'dev'",
            vec![],
            profile_grant("dev"),
        ),
        (
            dev,
            vec!["-s", "read-only", "--sandbox=workspace-write"],
            legacy_grant(&[]),
        ),
        (
            dev,
            vec![
                "-c",
                "sandbox_mode=workspace-write",
                "--config=sandbox_mode=read-only",
            ],
            vec![],
        ),
        (
            "",
            vec![
                "-c",
                "default_permissions=':read-only'",
                "--config",
                "default_permissions=dev",
            ],
            profile_grant("dev"),
        ),
        (
            "",
            vec![
                "-c",
                "default_permissions=dev",
                "-c",
                "sandbox_mode=read-only",
            ],
            profile_grant("dev"),
        ),
        (
            "[sandbox_workspace_write]\nwritable_roots = ['/base']",
            vec![
                "-c",
                "sandbox_workspace_write.writable_roots=['/first']",
                "--config=sandbox_workspace_write.writable_roots=['/last']",
            ],
            legacy_grant(&["/last"]),
        ),
    ] {
        std::fs::write(&config, content).unwrap();
        let mut args = overrides
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect::<Vec<_>>();
        CodexAdapter.configure_room_permissions(
            &mut args,
            &env,
            (home.path(), None),
            &roots,
            crate::config::Isolation::Host,
        );
        if args[..overrides.len()] != overrides || args[overrides.len()..] != expected {
            failures.push(format!(
                "config={content:?}, args={overrides:?}: appended {:?}, expected {expected:?}",
                &args[overrides.len()..]
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn provider_sandbox_defaults_only_without_a_selection_and_reads_profile_layers() {
    use crate::config::Isolation::{Host, Sandbox, Scoped};
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config.toml");
    let env = BTreeMap::from([("CODEX_HOME".into(), home.path().display().to_string())]);
    let roots = [
        PathBuf::from("/room/state"),
        PathBuf::from("/run/room"),
        PathBuf::from("/run/shared"),
    ];
    let legacy = "sandbox_workspace_write.writable_roots=";
    let profile = "permissions.dev.workspace_roots=";
    for isolation in [Host, Scoped, Sandbox] {
        for (content, flags, expected, forced) in [
            ("", vec![], legacy, isolation == Sandbox),
            ("sandbox_mode = 'danger-full-access'", vec![], "", false),
            (
                "default_permissions = ':danger-full-access'",
                vec![],
                "",
                false,
            ),
            ("", vec!["--sandbox=danger-full-access"], "", false),
            ("", vec!["-s", "danger-full-access"], "", false),
            (
                "",
                vec!["-c", "sandbox_mode='danger-full-access'"],
                "",
                false,
            ),
            (
                "",
                vec!["--config=default_permissions=':danger-full-access'"],
                "",
                false,
            ),
            ("sandbox_mode = 'read-only'", vec![], "", false),
            ("default_permissions = ':read-only'", vec![], "", false),
            ("", vec!["-s", "read-only"], "", false),
            ("", vec!["--sandbox=workspace-write"], legacy, false),
            ("", vec!["--approve-for-me"], legacy, false),
            ("", vec!["--not-so-yolo"], legacy, false),
            (
                "default_permissions = ':workspace'",
                vec![],
                "--add-dir",
                false,
            ),
            ("default_permissions = 'dev'", vec![], profile, false),
            ("", vec!["--yolo"], "", false),
            (
                "",
                vec!["--dangerously-bypass-approvals-and-sandbox"],
                "",
                false,
            ),
            (
                "sandbox_mode = 'danger-full-access'",
                vec!["--dangerously-bypass-approvals-and-sandbox"],
                "",
                false,
            ),
            (
                "sandbox_mode = 'danger-full-access'",
                vec!["--profile", "safe"],
                "",
                false,
            ),
            (
                "sandbox_mode = 'read-only'",
                vec!["--profile=unsafe"],
                "",
                false,
            ),
            (
                "sandbox_mode = 'danger-full-access'",
                vec!["-p", "workspace"],
                "--add-dir",
                false,
            ),
            (
                "sandbox_mode = 'danger-full-access'",
                vec!["-p=custom"],
                profile,
                false,
            ),
        ] {
            std::fs::write(&config, content).unwrap();
            for (name, contents) in [
                ("safe", "sandbox_mode = 'read-only'"),
                ("unsafe", "default_permissions = ':danger-full-access'"),
                ("workspace", "default_permissions = ':workspace'"),
                ("custom", "default_permissions = 'dev'"),
            ] {
                std::fs::write(home.path().join(format!("{name}.config.toml")), contents).unwrap();
            }
            let mut args = flags
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect::<Vec<_>>();
            CodexAdapter.configure_room_permissions(
                &mut args,
                &env,
                (home.path(), None),
                &roots,
                isolation,
            );
            assert_eq!(
                args.windows(2)
                    .filter(|pair| *pair == ["--sandbox", "workspace-write"])
                    .count(),
                usize::from(forced),
                "{isolation}, {content}, {flags:?}: {args:?}"
            );
            assert_eq!(
                &args[..flags.len()],
                flags,
                "user flags must survive: {args:?}"
            );
            assert_eq!(
                args.iter().any(|arg| arg.starts_with(legacy)
                    || arg.starts_with(profile)
                    || arg == "--add-dir"),
                !expected.is_empty(),
                "{isolation}, {content}, {flags:?}: {args:?}"
            );
            if !expected.is_empty() {
                assert!(args.iter().any(|arg| arg.starts_with(expected)), "{args:?}");
            } else {
                assert_eq!(args, flags, "no grant or selector: {args:?}");
            }
        }
    }
}

#[test]
fn native_subagent_denial_replaces_codex_multi_agent_overrides() {
    let tools = crate::agents::ToolSet::parse(&["Agent".into()]).unwrap();
    let rendered = render_definition_tools(&tools).unwrap();
    let mut args: Vec<String> = rendered
        .windows(2)
        .filter(|args| {
            args[0] == "-c"
                && (args[1].starts_with("agents.enabled=")
                    || args[1].starts_with("features.multi_agent"))
        })
        .flat_map(|args| args.iter().cloned())
        .collect();
    args.extend(
        [
            "-c=agents.enabled=true",
            "--config=features.multi_agent_v2={enabled=true}",
            "--config",
            "model_reasoning_effort=high",
            "--config",
            "agents.enabled=true",
            "--enable",
            "multi_agent_v2",
            "--enable=multi_agent_v2",
            "--enable",
            "other_feature",
            "-c",
            "features.multi_agent=true",
            "--model",
            "gpt-5",
        ]
        .map(ToOwned::to_owned),
    );

    CodexAdapter.disable_native_subagent_args(&mut args);

    assert_eq!(
        args,
        [
            "-c",
            "features.multi_agent=true",
            "--config",
            "model_reasoning_effort=high",
            "--enable",
            "other_feature",
            "-c",
            "features.multi_agent=true",
            "--model",
            "gpt-5",
            "-c",
            "agents.enabled=false",
            "-c",
            "features.multi_agent_v2=false",
        ]
        .map(ToOwned::to_owned)
    );
}

#[test]
fn provider_sandbox_respects_trusted_project_permission_layers() {
    use crate::config::Isolation;
    let home = tempfile::tempdir().unwrap();
    let project = home.path().join("project");
    let cwd = project.join("nested");
    for path in [
        project.join(".git"),
        project.join(".codex"),
        cwd.join(".codex"),
    ] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::write(project.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    let env = BTreeMap::from([("CODEX_HOME".into(), home.path().display().to_string())]);
    let roots = [
        PathBuf::from("/room/state"),
        PathBuf::from("/run/room"),
        PathBuf::from("/run/shared"),
    ];
    let root_key = project.display().to_string();
    for (base_mode, project_mode, nested_mode, trust, flags, forced) in [
        ("", "", "", "trusted", vec![], true),
        (
            "sandbox_mode = 'read-only'",
            "sandbox_mode = 'danger-full-access'",
            "",
            "untrusted",
            vec![],
            false,
        ),
        (
            "sandbox_mode = 'read-only'",
            "sandbox_mode = 'danger-full-access'",
            "",
            "trusted",
            vec![],
            false,
        ),
        (
            "sandbox_mode = 'read-only'",
            "default_permissions = ':danger-full-access'",
            "",
            "trusted",
            vec![],
            false,
        ),
        (
            "",
            "sandbox_mode = 'danger-full-access'",
            "sandbox_mode = 'read-only'",
            "trusted",
            vec![],
            false,
        ),
        (
            "",
            "sandbox_mode = 'read-only'",
            "sandbox_mode = 'danger-full-access'",
            "trusted",
            vec![],
            false,
        ),
        (
            "",
            "sandbox_mode = 'danger-full-access'",
            "",
            "untrusted",
            vec![],
            true,
        ),
        (
            "",
            "sandbox_mode = 'danger-full-access'",
            "",
            "trusted",
            vec!["--sandbox", "read-only"],
            false,
        ),
        (
            "",
            "sandbox_mode = 'danger-full-access'",
            "",
            "trusted",
            vec!["-c", "default_permissions=':read-only'"],
            false,
        ),
        (
            "",
            "sandbox_mode = 'danger-full-access'",
            "",
            "trusted",
            vec!["--profile", "safe"],
            false,
        ),
        (
            "sandbox_mode = 'danger-full-access'",
            "default_permissions = 'dev'",
            "",
            "trusted",
            vec![],
            false,
        ),
    ] {
        let key = toml::Value::String(root_key.clone());
        std::fs::write(
            home.path().join("config.toml"),
            format!("{base_mode}\n[projects.{key}]\ntrust_level = '{trust}'\n"),
        )
        .unwrap();
        std::fs::write(
            home.path().join("safe.config.toml"),
            "sandbox_mode = 'read-only'",
        )
        .unwrap();
        std::fs::write(project.join(".codex/config.toml"), project_mode).unwrap();
        std::fs::write(cwd.join(".codex/config.toml"), nested_mode).unwrap();
        let mut args = flags
            .iter()
            .map(|flag| (*flag).to_owned())
            .collect::<Vec<_>>();
        CodexAdapter.configure_room_permissions(
            &mut args,
            &env,
            (&cwd, None),
            &roots,
            Isolation::Sandbox,
        );
        assert_eq!(
            args.windows(2)
                .any(|pair| pair == ["--sandbox", "workspace-write"]),
            forced,
            "base={base_mode}, project={project_mode}, nested={nested_mode}, trust={trust}, flags={flags:?}: {args:?}"
        );
        assert_eq!(&args[..flags.len()], flags);
        if project_mode == "default_permissions = 'dev'" {
            assert!(
                args.iter()
                    .any(|arg| arg.starts_with("permissions.dev.workspace_roots=")),
                "{args:?}"
            );
        }
    }
}

#[test]
fn scoped_isolation_preserves_codex_sandbox_overrides() {
    let mut args = [
        "--sandbox",
        "workspace-write",
        "-s",
        "read-only",
        "--sandbox=workspace-write",
        "-c",
        "sandbox_mode=read-only",
        "--config=sandbox_mode=workspace-write",
        "-c",
        r#"default_permissions="rimz""#,
        "--config=default_permissions=other",
        "--config",
        "default_permissions=third",
        "-c",
        r#"approvals_reviewer="auto_review""#,
        "--ask-for-approval",
        "on-request",
    ]
    .map(ToOwned::to_owned)
    .to_vec();

    let original = args.clone();
    CodexAdapter.disable_native_sandbox_args(&mut args);
    assert_eq!(args, original);

    let mut ask = CodexAdapter
        .spec()
        .launch
        .permission_args(PermissionMode::Ask);
    CodexAdapter.disable_native_sandbox_args(&mut ask);
    assert_eq!(
        ask,
        [
            "--ask-for-approval",
            "on-request",
            "-c",
            r#"approvals_reviewer="user""#,
        ]
    );

    let mut approve_for_me = vec!["--not-so-yolo".to_owned(), "--approve-for-me".to_owned()];
    CodexAdapter.disable_native_sandbox_args(&mut approve_for_me);
    assert_eq!(approve_for_me, ["--not-so-yolo", "--approve-for-me"]);

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
