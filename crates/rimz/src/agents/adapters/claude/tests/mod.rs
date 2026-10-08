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
fn headless_result_reads_structured_verdict_cost_and_all_input_tokens() {
    let request = crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/unused/schema.json".into(),
        verdict_file: "/unused/verdict.json".into(),
    };
    let stdout = br#"{"structured_output":{"pass":false,"reason":"Nothing to do."},"result":"not the verdict","total_cost_usd":0.003,"usage":{"input_tokens":10,"cache_creation_input_tokens":20,"cache_read_input_tokens":30,"output_tokens":4},"is_error":false,"subtype":"success"}"#;
    let result = CLAUDE_DESCRIPTOR.launch.headless.unwrap().read_result(
        stdout,
        b"diagnostic on stderr",
        &request,
        Some("haiku"),
        &crate::agents::pricing::PriceBook::fixture(),
    );
    assert_eq!(
        result.verdict,
        Some(crate::agents::HeadlessVerdict {
            pass: false,
            reason: "Nothing to do.".into()
        })
    );
    assert_eq!(result.cost_usd, Some(0.003));
    assert_eq!(result.input_tokens, Some(60));
    assert_eq!(result.output_tokens, Some(4));
    assert_eq!(result.error, None);
    let noisy_stdout = [b"shell startup message\n".as_slice(), stdout, b"\n  \n"].concat();
    let noisy = CLAUDE_DESCRIPTOR.launch.headless.unwrap().read_result(
        &noisy_stdout,
        b"diagnostic on stderr",
        &request,
        Some("haiku"),
        &crate::agents::pricing::PriceBook::fixture(),
    );
    assert_eq!(
        noisy, result,
        "startup output must not hide the verdict or usage"
    );
}

#[test]
fn headless_result_rejects_malformed_and_provider_error_verdicts() {
    let request = crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: "/unused/schema.json".into(),
        verdict_file: "/unused/verdict.json".into(),
    };
    for stdout in [
        "not JSON",
        r#"{"result":"{\"pass\":true,\"reason\":\"No structured output\"}"}"#,
        r#"{"structured_output":{"pass":"true","reason":"wrong type"}}"#,
        r#"{"structured_output":{"pass":true}}"#,
        r#"{"structured_output":{"pass":true,"reason":"bad","extra":1}}"#,
        r#"{"is_error":true,"result":"quota exhausted","structured_output":{"pass":true,"reason":"must not trust"}}"#,
    ] {
        let result = CLAUDE_DESCRIPTOR.launch.headless.unwrap().read_result(
            stdout.as_bytes(),
            b"",
            &request,
            None,
            &crate::agents::pricing::PriceBook::fixture(),
        );
        assert!(result.error.is_some(), "{stdout}");
        assert_eq!(result.verdict, None);
    }
}

#[test]
fn headless_settings_merge_pending_host_skills_without_another_settings_flag() {
    let root = tempfile::tempdir().unwrap();
    let mut args = vec!["--settings".into(), r#"{"theme":"dark"}"#.into()];
    let mut artifact = merge_settings(root.path(), root.path(), &mut args, None, true, |object| {
        object.insert(
            "skillOverrides".into(),
            json!({"unlisted":"user-invocable-only"}),
        );
        Ok(())
    })
    .unwrap();
    let request = crate::agents::HeadlessRequest {
        schema: crate::agents::CHECK_VERDICT_SCHEMA.into(),
        schema_file: root.path().join("schema.json"),
        verdict_file: root.path().join("verdict.json"),
    };
    let argv = CLAUDE_DESCRIPTOR
        .launch
        .headless
        .unwrap()
        .render_argv(
            &args,
            Some("Decide."),
            &request,
            root.path(),
            (root.path(), &mut artifact),
        )
        .unwrap();
    assert_eq!(argv.iter().filter(|arg| *arg == "--settings").count(), 1);
    let (_, settings, _) = artifact.unwrap();
    assert_eq!(settings["disableAllHooks"], true);
    assert_eq!(settings["theme"], "dark");
    assert_eq!(
        settings["skillOverrides"]["unlisted"],
        "user-invocable-only"
    );
}

#[test]
fn deadline_context_reply_matches_native_post_tool_contract() {
    let (post, other) = crate::agents::testkit::deadline_context_replies("claude", "PostToolUse");
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
        crate::agents::testkit::prompt_context_stdout("claude", "UserPromptSubmit"),
        @r#"{"hookSpecificOutput":{"additionalContext":"prompt context","hookEventName":"UserPromptSubmit"}}"#
    );
}

#[test]
fn routine_rimz_settings_union_and_idempotence() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    let provider = root.path().join("provider");
    let library = root.path().join("library");
    for (base, name) in [
        (&provider, "rimz-a"),
        (&provider, "rimz-b"),
        (&provider, "other"),
        (&library, "rimz-d"),
        (&provider, "rimz-*"),
    ] {
        std::fs::create_dir_all(base.join(name)).unwrap();
        std::fs::write(
            base.join(name).join("SKILL.md"),
            "---\nname: different\n---\n",
        )
        .unwrap();
    }
    std::fs::create_dir(provider.join("rimz-c")).unwrap();
    let dirs = [root.path().join("tmp"), root.path().join("shared")];
    for profile in [
        None,
        Some(
            json!({"permissions":{"allow":["Bash(custom *)"],"additionalDirectories":["/custom"]},"autoMode":{"environment":["custom","custom"],"allow":["unchanged"]}}),
        ),
    ] {
        let mut args = profile
            .as_ref()
            .map(|value| vec!["--settings".into(), value.to_string()])
            .unwrap_or_default();
        let mut artifact = None;
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (Some(&provider), Some(&library)),
                &dirs,
                &mut args,
                &mut artifact,
            )
            .unwrap();
        assert_eq!(args.len(), 2);
        assert!(artifact.is_none());
        let value: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
        let prefixes = [
            "message",
            "agents",
            "subagents",
            "teams",
            "asks",
            "answer",
            "wait --in",
            "pane list",
            "pane capture",
            "loop show",
            "loop logs",
            "lsp def",
            "lsp refs",
            "lsp hover",
            "lsp impl",
            "lsp callers",
            "lsp callees",
            "lsp symbols",
            "lsp find",
            "lsp check",
            "lsp list",
            "lsp status",
        ];
        let mut expected: Vec<_> = prefixes
            .iter()
            .map(|prefix| json!(format!("Bash(rimz {prefix} *)")))
            .collect();
        if profile.is_some() {
            expected.insert(0, json!("Bash(custom *)"));
        }
        expected.extend([
            json!("Skill(rimz-a)"),
            json!("Skill(rimz-b)"),
            json!("Skill(rimz-d)"),
        ]);
        assert_eq!(value["permissions"]["allow"], json!(expected));
        let env = value["autoMode"]["environment"].as_array().unwrap();
        assert_eq!(env.iter().filter(|entry| **entry == "$defaults").count(), 1);
        let text = env.last().unwrap().as_str().unwrap();
        for name in [
            format!("$TMPDIR ({})", dirs[0].display()),
            format!("$RIMZ_SHARED ({})", dirs[1].display()),
        ] {
            assert!(text.contains(&name), "{text}");
        }
        let mut expected_dirs = vec![json!(dirs[0]), json!(dirs[1])];
        if profile.is_some() {
            expected_dirs.insert(0, json!("/custom"));
            assert_eq!(value["autoMode"]["allow"], json!(["unchanged"]));
            assert_eq!(env.iter().filter(|entry| **entry == "custom").count(), 1);
        } else {
            assert!(value["autoMode"].get("allow").is_none());
        }
        assert_eq!(
            value["permissions"]["additionalDirectories"],
            json!(expected_dirs)
        );
        let before = args.clone();
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (Some(&provider), Some(&library)),
                &dirs,
                &mut args,
                &mut artifact,
            )
            .unwrap();
        assert_eq!(args, before);
    }
}

#[test]
fn routine_rimz_file_and_pending_skills_artifact() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    let provider = root.path().join("provider");
    let library = root.path().join("library");
    std::fs::write(
        root.path().join("profile.json"),
        "{ // jsonc\n\"env\":{\"TOKEN\":\"private-secret\"},}",
    )
    .unwrap();
    let dirs = [root.path().join("tmp"), root.path().join("shared")];
    for skills in [false, true] {
        let mut args = vec![
            "--settings={}".into(),
            "--settings".into(),
            "profile.json".into(),
        ];
        let mut artifact = if skills {
            render_host_skills(&[], root.path(), root.path(), &mut args).unwrap()
        } else {
            None
        };
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (Some(&provider), Some(&library)),
                &dirs,
                &mut args,
                &mut artifact,
            )
            .unwrap();
        let (path, value, _) = artifact.as_ref().unwrap();
        assert_eq!(args, ["--settings", path.to_str().unwrap()]);
        assert!(!path.exists());
        assert!(!args.join(" ").contains("private-secret"));
        assert_eq!(value["env"]["TOKEN"], "private-secret");
        assert_eq!(value["skillOverrides"].is_object(), skills);
        assert!(value["permissions"]["allow"].is_array());
        let before = (args.clone(), artifact.clone());
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (Some(&provider), Some(&library)),
                &dirs,
                &mut args,
                &mut artifact,
            )
            .unwrap();
        assert_eq!((&args, &artifact), (&before.0, &before.1));
        let (path, value, _) = artifact.as_ref().unwrap();
        std::fs::write(path, value.to_string()).unwrap();
        artifact = None;
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (Some(&provider), Some(&library)),
                &dirs,
                &mut args,
                &mut artifact,
            )
            .unwrap();
        assert_eq!(args, before.0);
        assert_eq!(
            artifact.map(|(path, value, _)| (path, value)),
            before.1.map(|(path, value, _)| (path, value))
        );
    }
}

#[test]
fn routine_rimz_invalid_settings_refuse_with_fix() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    for (contents, key) in [
        ("{broken", "JSON"),
        ("[]", "object"),
        (r#"{"permissions":{"allow":null}}"#, "permissions.allow"),
        (
            r#"{"permissions":{"additionalDirectories":{}}}"#,
            "permissions.additionalDirectories",
        ),
        (
            r#"{"autoMode":{"environment":false}}"#,
            "autoMode.environment",
        ),
        (r#"{"permissions":false}"#, "permissions"),
        (r#"{"autoMode":[]}"#, "autoMode"),
    ] {
        std::fs::write(root.path().join("profile.json"), contents).unwrap();
        let mut args = vec!["--settings".into(), "profile.json".into()];
        let error = ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (None, None),
                &[root.path().join("scratch"), root.path().join("shared")],
                &mut args,
                &mut None,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("profile.json"), "{error}");
        assert!(error.contains(key), "{error}");
        assert!(
            error.ends_with("correct that key, or set allow-routine-rimz = false"),
            "{error}"
        );
    }
}

#[test]
fn listed_skills_union_private_artifact_and_empty_rules() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    let listed = ["commit", "rimz-lsp", "bad*"].map(|name| name.parse().unwrap());
    let profile = json!({"env":{"TOKEN":"private-secret"}, "permissions":{"allow":["Bash(custom *)", "Skill(rimz-lsp)"]}});
    std::fs::write(root.path().join("profile.json"), profile.to_string()).unwrap();
    for file in [false, true] {
        let mut args = vec![
            "--settings".into(),
            if file {
                "profile.json".into()
            } else {
                profile.to_string()
            },
        ];
        let mut artifact = None;
        ClaudeAdapter
            .allow_listed_skill_args(
                &listed,
                (root.path(), root.path()),
                &mut args,
                &mut artifact,
            )
            .unwrap();
        let value = if file {
            let (path, value, _) = artifact.as_ref().unwrap();
            assert_eq!(args, ["--settings", path.to_str().unwrap()]);
            assert!(
                path.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("settings.")
            );
            assert!(!args.join(" ").contains("private-secret"));
            value.clone()
        } else {
            serde_json::from_str(&args[1]).unwrap()
        };
        assert_eq!(
            value["permissions"]["allow"],
            json!(["Bash(custom *)", "Skill(rimz-lsp)", "Skill(commit)"])
        );
        assert_eq!(value["env"], profile["env"]);
        let before = (args.clone(), artifact.clone());
        ClaudeAdapter
            .allow_listed_skill_args(
                &listed,
                (root.path(), root.path()),
                &mut args,
                &mut artifact,
            )
            .unwrap();
        assert_eq!((args, artifact), before);
    }
    for listed in [vec![], vec!["bad*".parse().unwrap()]] {
        let mut args = vec!["--model".into(), "sonnet".into()];
        let before = args.clone();
        ClaudeAdapter
            .allow_listed_skill_args(&listed, (root.path(), root.path()), &mut args, &mut None)
            .unwrap();
        assert_eq!(args, before);
    }
}

#[test]
fn allowed_tools_union_profile_skill_and_routine_rules_in_private_artifacts() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    let profile = json!({"env":{"TOKEN":"private-secret"}, "permissions":{"allow":["Read", "Skill(commit)", "Bash(rimz agents *)"]}});
    std::fs::write(root.path().join("profile.json"), profile.to_string()).unwrap();
    let rules =
        ["Bash(git *)", "Skill(commit)", "Bash(rimz agents *)"].map(|rule| rule.parse().unwrap());
    for file in [false, true] {
        let mut args = vec![
            "--settings".into(),
            if file {
                "profile.json".into()
            } else {
                profile.to_string()
            },
        ];
        let mut artifact = None;
        ClaudeAdapter
            .allow_listed_skill_args(
                &["commit".parse().unwrap()],
                (root.path(), root.path()),
                &mut args,
                &mut artifact,
            )
            .unwrap();
        render_tool_rules(&rules, (root.path(), root.path()), &mut args, &mut artifact).unwrap();
        let value = if file {
            let (path, value, _) = artifact.as_ref().unwrap();
            assert_eq!(args, ["--settings", path.to_str().unwrap()]);
            assert!(!args.join(" ").contains("private-secret"));
            value.clone()
        } else {
            serde_json::from_str(&args[1]).unwrap()
        };
        assert_eq!(
            value["permissions"]["allow"],
            json!([
                "Read",
                "Skill(commit)",
                "Bash(rimz agents *)",
                "Bash(git *)"
            ])
        );
        assert_eq!(value["env"], profile["env"]);
        let before = (args.clone(), artifact.clone());
        render_tool_rules(&rules, (root.path(), root.path()), &mut args, &mut artifact).unwrap();
        assert_eq!((args.clone(), artifact.clone()), before);
        ClaudeAdapter
            .allow_routine_rimz_args(
                (root.path(), root.path()),
                (None, None),
                &[root.path().join("scratch"), root.path().join("shared")],
                &mut args,
                &mut artifact,
            )
            .unwrap();
        let merged = artifact
            .as_ref()
            .map(|(_, value, _)| value.clone())
            .unwrap_or_else(|| serde_json::from_str(&args[1]).unwrap());
        let allow = merged["permissions"]["allow"].as_array().unwrap();
        for rule in [
            "Read",
            "Skill(commit)",
            "Bash(rimz agents *)",
            "Bash(git *)",
        ] {
            assert_eq!(
                allow.iter().filter(|value| **value == json!(rule)).count(),
                1
            );
        }
    }
}

#[test]
fn allowed_tools_invalid_settings_refuse_with_own_fix_and_empty_is_inert() {
    let root = tempfile::tempdir().unwrap();
    for profile in [
        r#"{"permissions":false}"#,
        r#"{"permissions":{"allow":false}}"#,
    ] {
        std::fs::write(root.path().join("profile.json"), profile).unwrap();
        let mut args = vec!["--settings".into(), "profile.json".into()];
        let before = args.clone();
        render_tool_rules(&[], (root.path(), root.path()), &mut args, &mut None).unwrap();
        assert_eq!(args, before);
        let error = render_tool_rules(
            &["Read".parse().unwrap()],
            (root.path(), root.path()),
            &mut args,
            &mut None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("profile.json") && error.contains("permissions"),
            "{error}"
        );
        assert!(
            error.ends_with("correct that key, or remove the definition's allowed-tools list"),
            "{error}"
        );
    }
}

#[test]
fn listed_skills_invalid_settings_refuse_with_own_fix() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("profile.json"), r#"{"permissions":false}"#).unwrap();
    for host in [false, true] {
        let mut args = vec!["--settings".into(), "profile.json".into()];
        let mut artifact = if host {
            render_host_skills(&[], root.path(), root.path(), &mut args).unwrap()
        } else {
            None
        };
        let error = ClaudeAdapter
            .allow_listed_skill_args(
                &["commit".parse().unwrap()],
                (root.path(), root.path()),
                &mut args,
                &mut artifact,
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("profile.json"), "{error}");
        assert!(error.contains("permissions"), "{error}");
        assert!(
            error.ends_with("correct that key, or remove the profile's skills list"),
            "{error}"
        );
        assert!(!error.contains("allow-routine-rimz"), "{error}");
    }
}

#[test]
fn routine_skills_unreadable_root_refuses_with_fix() {
    use crate::agents::capabilities::LaunchCapability;
    let root = tempfile::tempdir().unwrap();
    let bad = root.path().join("not-directory");
    std::fs::write(&bad, "file").unwrap();
    let error = ClaudeAdapter
        .allow_routine_rimz_args(
            (root.path(), root.path()),
            (Some(&bad), None),
            &[root.path().join("scratch"), root.path().join("shared")],
            &mut vec![],
            &mut None,
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains(bad.to_str().unwrap()), "{error}");
    assert!(
        error.ends_with("or set allow-routine-rimz = false"),
        "{error}"
    );
}

#[test]
fn routine_skills_ignore_unreadable_non_rimz_dirs() {
    use crate::agents::capabilities::LaunchCapability;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let provider = root.path().join("provider");
    for name in ["rimz-x", "other"] {
        std::fs::create_dir_all(provider.join(name)).unwrap();
        std::fs::write(provider.join(name).join("SKILL.md"), "---\n---\n").unwrap();
    }
    let other = provider.join("other");
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o000)).unwrap();
    let denied = other.join("SKILL.md").try_exists().is_err();
    let mut args = Vec::new();
    let result = ClaudeAdapter.allow_routine_rimz_args(
        (root.path(), root.path()),
        (Some(&provider), None),
        &[root.path().join("scratch"), root.path().join("shared")],
        &mut args,
        &mut None,
    );
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
    if !denied {
        return; // Running as root: mode 000 denies nothing, so there is no unreadable entry to skip.
    }
    result.unwrap();
    let value: serde_json::Value = serde_json::from_str(&args[1]).unwrap();
    let allow = value["permissions"]["allow"].as_array().unwrap();
    assert!(allow.contains(&json!("Skill(rimz-x)")));
    assert!(!allow.contains(&json!("Skill(other)")));
}

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
    let (artifact_path, merged, _) = &artifact;
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
    assert_eq!(
        inline_artifact.map(|(path, value, _)| (path, value)),
        Some((artifact.0, artifact.1))
    );
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
