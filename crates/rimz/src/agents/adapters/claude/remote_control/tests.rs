use serde_json::json;

use super::*;

fn assert_traffic_block(variable: &'static str, in_settings: bool) {
    for version in [None, v(50), v(296)] {
        let settings = if in_settings {
            settings(json!({ "env": { (variable): "1" } }))
        } else {
            readiness_settings()
        };
        let issue = readiness_from(
            version,
            settings_path(),
            settings,
            false,
            false,
            false,
            false,
            consent(remote_consent::ConsentState::Seeded),
            (!in_settings).then_some(variable),
            None,
        )
        .expect_err("traffic switches must block remote control")
        .to_string();
        assert!(issue.contains(variable), "{issue}");
        assert!(issue.contains("[remote_control] claude = false"), "{issue}");
        let source = if in_settings {
            "/home/u/.claude/settings.json"
        } else {
            "launch environment"
        };
        assert!(issue.contains(source), "{issue}");
    }
}

#[test]
fn nonessential_traffic_launch_switch_blocks_readiness() {
    assert_traffic_block("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", false);
}

#[test]
fn nonessential_traffic_settings_switch_blocks_readiness() {
    assert_traffic_block("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", true);
}

#[test]
fn growthbook_launch_switch_blocks_readiness() {
    assert_traffic_block("DISABLE_GROWTHBOOK", false);
}

#[test]
fn growthbook_settings_switch_blocks_readiness() {
    assert_traffic_block("DISABLE_GROWTHBOOK", true);
}

#[test]
fn pane_auto_status_honors_traffic_switches() {
    for variable in TRAFFIC_SWITCHES {
        let configured = settings(json!({
            "remoteControlAtStartup": true,
            "env": { (variable): "1" }
        }));
        assert!(!pane_auto_enabled_from(
            &configured,
            None,
            false,
            false,
            None
        ));
        let enabled = settings(json!({ "remoteControlAtStartup": true }));
        assert!(pane_auto_enabled_from(&enabled, None, false, false, None));
        assert!(!pane_auto_enabled_from(
            &enabled,
            None,
            false,
            false,
            Some(variable)
        ));
    }
}

#[test]
fn folder_trust_blocks_only_an_undecided_project() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("project");
    let home = dir.path().join("claude");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let env = BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), home.display().to_string())]);
    let check = |project_root: Option<&std::path::Path>| {
        readiness_from(
            None,
            settings_path(),
            readiness_settings(),
            false,
            false,
            false,
            false,
            consent(remote_consent::ConsentState::Seeded),
            None,
            project_root
                .map(|root| super::super::folder_trust::folder_trust(root, Some(root), &env)),
        )
    };
    assert!(
        check(None).is_ok(),
        "machine-wide preflight skips folder trust"
    );
    let issue = check(Some(&root))
        .expect_err("an untrusted host would wait on a prompt")
        .to_string();
    assert!(
        issue.contains("rimz trust grant --agents claude"),
        "{issue}"
    );
    assert!(issue.contains(root.to_str().unwrap()), "{issue}");
    std::fs::write(
        remote_consent::global_config_path(&env).unwrap(),
        serde_json::to_vec(&json!({ "projects": { (root.to_str().unwrap()): { "hasTrustDialogAccepted": true } } })).unwrap(),
    ).unwrap();
    assert!(check(Some(&root)).is_ok(), "a granted folder can serve");
}

fn settings(value: Value) -> ClaudeRcSettings {
    rc_settings_from(value.as_object().expect("object"))
}

#[test]
fn remote_session_env_accepts_each_upstream_marker() {
    assert!(remote_session_env(Some(OsStr::new("token")), None));
    assert!(remote_session_env(None, Some(OsStr::new("bridge"))));
    assert!(!remote_session_env(Some(OsStr::new("")), None));
    assert!(!remote_session_env(None, Some(OsStr::new("local"))));
    assert!(!remote_session_env(None, None));
}

#[test]
fn remote_session_ancestry_matches_only_a_claude_remote_control_host() {
    let remote = |pid| match pid {
        30 => Some("/versions/claude --print --sdk-url wss://example".to_owned()),
        20 => Some("/usr/local/bin/claude remote-control --spawn worktree".to_owned()),
        _ => None,
    };
    assert!(claude_host_in(vec![30, 20, 10], remote));

    let normal = |pid| match pid {
        30 => Some("/usr/local/bin/claude --resume session".to_owned()),
        20 => Some("zsh".to_owned()),
        _ => None,
    };
    assert!(!claude_host_in(vec![30, 20, 10], normal));
}

#[test]
fn rc_settings_from_reads_flags_conflicts_and_ignores_empties() {
    let parsed = settings(json!({
        "disableRemoteControl": true,
        "remoteControlAtStartup": true,
    }));
    assert!(parsed.disable_remote_control);
    assert!(parsed.remote_control_at_startup);

    // An apiKeyHelper or an env auth token (either key) is an auth conflict.
    assert!(settings(json!({ "apiKeyHelper": "op read key" })).api_key_helper);
    assert!(settings(json!({ "env": { "ANTHROPIC_API_KEY": "sk-ant" } })).env_auth_conflict);
    assert!(settings(json!({ "env": { "ANTHROPIC_AUTH_TOKEN": "token" } })).env_auth_conflict);
    assert!(settings(json!({ "env": { "CLAUDE_CODE_OAUTH_TOKEN": "token" } })).oauth_token_env);
    assert!(
        settings(json!({ "env": { "ANTHROPIC_BASE_URL": "https://gateway.example" } }))
            .env_endpoint_conflict
    );
    assert!(settings(json!({ "env": { "CLAUDE_CODE_USE_BEDROCK": "1" } })).env_endpoint_conflict);
    assert!(
        !settings(json!({ "env": { "ANTHROPIC_BASE_URL": "https://api.anthropic.com/" } }))
            .env_endpoint_conflict
    );

    // Falsey, empty, and empty-env values read as the default (nothing set).
    assert_eq!(
        settings(json!({
            "disableRemoteControl": false,
            "remoteControlAtStartup": false,
            "apiKeyHelper": "",
            "env": {
                "ANTHROPIC_API_KEY": "",
                "CLAUDE_CODE_OAUTH_TOKEN": ""
            }
        })),
        ClaudeRcSettings::default()
    );
}

#[test]
fn pane_auto_status_suppresses_settings_disabled_cases() {
    let disabled = ClaudeRcSettings {
        remote_control_at_startup: true,
        disable_remote_control: true,
        ..ClaudeRcSettings::default()
    };
    assert!(!pane_auto_enabled_from(&disabled, None, false, false, None));

    let off = ClaudeRcSettings {
        remote_control_at_startup: false,
        ..ClaudeRcSettings::default()
    };
    assert!(!pane_auto_enabled_from(&off, None, false, false, None));
}

#[test]
fn pane_auto_status_uses_account_version_for_auth_gate() {
    let conflict = ClaudeRcSettings {
        remote_control_at_startup: true,
        api_key_helper: true,
        ..ClaudeRcSettings::default()
    };

    assert!(
        pane_auto_enabled_from(&conflict, None, false, false, None),
        "unknown versions apply only version-independent gates"
    );
    assert!(pane_auto_enabled_from(
        &conflict,
        Some(CliVersion::new(2, 1, 156)),
        false,
        false,
        None,
    ));
    assert!(!pane_auto_enabled_from(
        &conflict,
        Some(CliVersion::new(2, 1, 157)),
        false,
        false,
        None,
    ));
}

#[test]
fn pane_auto_status_applies_the_custom_endpoint_gate_readiness_applies() {
    let endpoint = ClaudeRcSettings {
        remote_control_at_startup: true,
        env_endpoint_conflict: true,
        ..ClaudeRcSettings::default()
    };

    assert!(pane_auto_enabled_from(&endpoint, None, false, false, None));
    assert!(pane_auto_enabled_from(
        &endpoint,
        Some(CliVersion::new(2, 1, 195)),
        false,
        false,
        None,
    ));
    assert!(!pane_auto_enabled_from(
        &endpoint,
        Some(CUSTOM_ENDPOINT_BLOCKS_RC_SINCE),
        false,
        false,
        None,
    ));
    assert!(
        readiness_from(
            Some(CUSTOM_ENDPOINT_BLOCKS_RC_SINCE),
            PathBuf::from("settings.json"),
            endpoint,
            false,
            false,
            false,
            false,
            None,
            None,
            None,
        )
        .is_err()
    );
}

fn readiness_settings() -> ClaudeRcSettings {
    ClaudeRcSettings::default()
}

fn settings_path() -> PathBuf {
    PathBuf::from("/home/u/.claude/settings.json")
}

fn v(patch: u64) -> Option<CliVersion> {
    Some(CliVersion::new(2, 1, patch))
}

fn consent(state: remote_consent::ConsentState) -> Option<(PathBuf, remote_consent::ConsentState)> {
    Some((PathBuf::from("/home/u/.claude.json"), state))
}

fn decision(
    version: Option<CliVersion>,
    settings: ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
    env_oauth_token: bool,
) -> Result<Vec<String>, Issue> {
    readiness_from(
        version,
        settings_path(),
        settings,
        env_api_key,
        env_auth_token,
        env_oauth_token,
        false,
        consent(remote_consent::ConsentState::Seeded),
        None,
        None,
    )
}

#[test]
fn host_argv_uses_worktree_spawn() {
    let argv = host_argv();
    assert_eq!(
        argv,
        vec!["claude", "remote-control", "--spawn", "worktree"]
    );
    assert!(crate::pane::command_is_host(&argv.join(" ")));
}

#[test]
fn host_argv_preserves_login_and_host_identity() {
    let env = BTreeMap::from([("CLAUDE_CONFIG_DIR".to_owned(), "/srv/work".to_owned())]);
    let argv = host_argv_with_login(host_argv(), &env);
    assert_eq!(
        argv,
        vec![
            "env",
            "CLAUDE_CONFIG_DIR=/srv/work",
            "claude",
            "remote-control",
            "--spawn",
            "worktree"
        ]
    );
    assert!(crate::pane::command_is_host(&argv.join(" ")));
    assert_eq!(
        host_argv_with_login(host_argv(), &BTreeMap::new()),
        host_argv()
    );
}

#[test]
fn an_unrecorded_dialog_blocks_before_any_version_or_auth_gate() {
    // An unanswered prompt holds the pane open, so it outranks every gate
    // that only degrades the host.
    let blocked = readiness_from(
        None,
        settings_path(),
        readiness_settings(),
        true,
        true,
        true,
        true,
        consent(remote_consent::ConsentState::Unseeded),
        None,
        None,
    );
    assert_eq!(
        blocked,
        Err(Issue::ConsentPending {
            config_path: PathBuf::from("/home/u/.claude.json"),
        })
    );
}

#[test]
fn an_explicit_refusal_is_reported_rather_than_overridden() {
    assert_eq!(
        readiness_from(
            v(215),
            settings_path(),
            readiness_settings(),
            false,
            false,
            false,
            false,
            consent(remote_consent::ConsentState::Refused),
            None,
            None,
        ),
        Err(Issue::ConsentRefused {
            config_path: PathBuf::from("/home/u/.claude.json"),
        })
    );
}

#[test]
fn an_unreadable_global_config_leaves_the_host_launchable() {
    // Claude owns that file and may still recover it; refusing the room
    // over a file RimZ cannot parse would be worse than letting it try.
    assert!(
        readiness_from(
            v(215),
            settings_path(),
            readiness_settings(),
            false,
            false,
            false,
            false,
            consent(remote_consent::ConsentState::Unreadable),
            None,
            None,
        )
        .is_ok()
    );
}

#[test]
fn consent_issues_keep_claude_setup_guidance() {
    let pending = Issue::ConsentPending {
        config_path: PathBuf::from("/home/u/.claude.json"),
    }
    .to_string();
    assert!(pending.contains("[remote_control] claude"));
    assert!(pending.contains("remoteDialogSeen"));
    assert!(pending.contains("/home/u/.claude.json"));

    let refused = Issue::ConsentRefused {
        config_path: PathBuf::from("/home/u/.claude.json"),
    }
    .to_string();
    assert!(refused.contains("[remote_control] claude"));
    assert!(refused.contains("remoteDialogSeen"));
}

#[test]
fn issues_keep_claude_setup_guidance() {
    let issue = Issue::TooOld {
        found: CliVersion::new(2, 1, 50),
    }
    .to_string();
    assert!(issue.contains("[remote_control] claude"));
    assert!(issue.contains(">= 2.1.51"));
}

#[test]
fn readiness_blocks_old_versions_and_disabled_settings() {
    assert_eq!(
        decision(v(50), readiness_settings(), false, false, false),
        Err(Issue::TooOld {
            found: CliVersion::new(2, 1, 50)
        })
    );

    let settings = ClaudeRcSettings {
        disable_remote_control: true,
        ..readiness_settings()
    };
    assert_eq!(
        decision(v(173), settings, false, false, false),
        Err(Issue::RemoteControlDisabled {
            settings_path: settings_path()
        })
    );
}

#[test]
fn readiness_auth_conflict_gate_starts_at_2_1_157() {
    let settings = ClaudeRcSettings {
        api_key_helper: true,
        env_auth_conflict: true,
        ..readiness_settings()
    };
    assert!(decision(v(156), settings.clone(), true, true, true).is_ok());
    assert_eq!(
        decision(v(157), settings, true, true, true),
        Err(Issue::AuthConflict {
            sources: vec![
                AuthConflictSource::ApiKeyEnv,
                AuthConflictSource::AuthTokenEnv,
                AuthConflictSource::OAuthTokenEnv,
                AuthConflictSource::ApiKeyHelperSetting,
                AuthConflictSource::SettingsEnv,
            ]
        })
    );
}

#[test]
fn readiness_blocks_long_lived_oauth_tokens_from_env_or_settings() {
    assert_eq!(
        decision(v(247), readiness_settings(), false, false, true),
        Err(Issue::AuthConflict {
            sources: vec![AuthConflictSource::OAuthTokenEnv],
        })
    );

    let settings = ClaudeRcSettings {
        oauth_token_env: true,
        ..readiness_settings()
    };
    assert_eq!(
        decision(v(247), settings, false, false, false),
        Err(Issue::AuthConflict {
            sources: vec![AuthConflictSource::OAuthTokenEnv],
        })
    );

    let guidance = Issue::AuthConflict {
        sources: vec![AuthConflictSource::OAuthTokenEnv],
    }
    .to_string();
    assert!(guidance.contains("CLAUDE_CODE_OAUTH_TOKEN"));
    assert!(guidance.contains("claude auth login"));
}

#[test]
fn readiness_custom_endpoint_gate_starts_at_2_1_196() {
    let settings = ClaudeRcSettings {
        env_endpoint_conflict: true,
        ..readiness_settings()
    };
    assert!(
        readiness_from(
            v(195),
            settings_path(),
            settings.clone(),
            false,
            false,
            false,
            true,
            consent(remote_consent::ConsentState::Seeded),
            None,
            None,
        )
        .is_ok()
    );
    assert_eq!(
        readiness_from(
            v(196),
            settings_path(),
            settings,
            false,
            false,
            false,
            true,
            consent(remote_consent::ConsentState::Seeded),
            None,
            None,
        ),
        Err(Issue::AuthConflict {
            sources: vec![
                AuthConflictSource::EndpointEnv,
                AuthConflictSource::SettingsEndpoint,
            ]
        })
    );
}

#[test]
fn unknown_version_applies_only_settings_independent_gate() {
    let settings = ClaudeRcSettings {
        api_key_helper: true,
        ..readiness_settings()
    };
    assert!(decision(None, settings, true, false, true).is_ok());

    let settings = ClaudeRcSettings {
        disable_remote_control: true,
        ..readiness_settings()
    };
    assert_eq!(
        decision(None, settings, false, false, false),
        Err(Issue::RemoteControlDisabled {
            settings_path: settings_path()
        })
    );
}
