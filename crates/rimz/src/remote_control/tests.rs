use super::*;
use crate::ids::{MuxName, PaneId};

fn pane(command: Option<&str>, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, "%1"),
        session_name: "rimz-demo".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: view_name.map(ToOwned::to_owned),
        is_focused: false,
        is_floating: false,
        command: command.map(ToOwned::to_owned),
        spawn_command: None,
        cwd: None,
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

#[test]
fn claude_command_uses_worktree_spawn() {
    assert_eq!(
        claude_command(),
        vec!["claude", "remote-control", "--spawn", "worktree"],
    );
}

#[test]
fn claude_host_argv_uses_the_documented_server_command() {
    let argv = claude_host_argv();
    assert_eq!(argv, ["claude", "remote-control", "--spawn", "worktree"]);
    assert!(command_is_host(&argv.join(" ")));
}

#[test]
fn codex_command_runs_the_standalone_bin() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    assert_eq!(
        codex_command(bin),
        vec![
            "/home/u/.codex/packages/standalone/current/codex",
            "remote-control",
            "start",
        ],
    );
}

#[test]
fn standalone_bin_resolves_only_when_the_install_exists() {
    let home = tempfile::tempdir().expect("tempdir");
    // Absent install → no host: `remote-control start` would only error.
    assert!(standalone_bin_under(home.path()).is_none());

    let bin = home
        .path()
        .join("packages")
        .join("standalone")
        .join("current")
        .join("codex");
    std::fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
    std::fs::write(&bin, b"#!/bin/sh\n").expect("write");
    assert_eq!(standalone_bin_under(home.path()), Some(bin));
}

#[test]
fn preflight_blocks_only_codex_without_its_standalone() {
    // codex off → never blocks, install present or not.
    assert!(preflight_decision(false, false).is_ok());
    assert!(preflight_decision(false, true).is_ok());
    // codex on → blocks iff the standalone install is absent.
    assert_eq!(
        preflight_decision(true, false),
        Err(PreflightError::CodexStandaloneMissing),
    );
    assert!(preflight_decision(true, true).is_ok());
}

#[test]
fn start_decision_skips_uninstalled_hosts_but_keeps_hard_refusals() {
    assert_eq!(
        start_decision(Err(PreflightError::CodexStandaloneMissing), Ok(())),
        Ok(()),
    );
    assert_eq!(
        start_decision(
            Err(PreflightError::CodexStandaloneMissing),
            Err(PreflightError::ClaudeTooOld {
                found: CliVersion::new(2, 1, 50),
            }),
        ),
        Err(PreflightError::ClaudeTooOld {
            found: CliVersion::new(2, 1, 50),
        }),
    );
    assert_eq!(
        start_decision(
            Ok(()),
            Err(PreflightError::ClaudeRemoteControlDisabled {
                settings_path: settings_path(),
            }),
        ),
        Err(PreflightError::ClaudeRemoteControlDisabled {
            settings_path: settings_path(),
        }),
    );
    assert_eq!(start_decision(Ok(()), Ok(())), Ok(()));
}

fn claude_settings() -> claude_rc::ClaudeRcSettings {
    claude_rc::ClaudeRcSettings::default()
}

fn settings_path() -> PathBuf {
    PathBuf::from("/home/u/.claude/settings.json")
}

fn v(patch: u64) -> Option<CliVersion> {
    Some(CliVersion::new(2, 1, patch))
}

fn claude_decision(
    version: Option<CliVersion>,
    settings: claude_rc::ClaudeRcSettings,
    env_api_key: bool,
    env_auth_token: bool,
) -> Result<(), PreflightError> {
    claude_preflight_decision(
        true,
        true,
        version,
        settings_path(),
        settings,
        env_api_key,
        env_auth_token,
        false,
    )
}

#[test]
fn only_codex_standalone_missing_is_an_uninstalled_host() {
    assert!(PreflightError::CodexStandaloneMissing.is_uninstalled_host());

    let hard_refusals = [
        PreflightError::ClaudeTooOld {
            found: CliVersion::new(2, 1, 50),
        },
        PreflightError::ClaudeRemoteControlDisabled {
            settings_path: settings_path(),
        },
        PreflightError::ClaudeAuthConflict {
            sources: vec![ClaudeAuthConflictSource::ApiKeyEnv],
        },
    ];
    for refusal in hard_refusals {
        assert!(!refusal.is_uninstalled_host(), "{refusal:?}");
    }
}

#[test]
fn claude_preflight_preserves_off_and_absent_binary_skip() {
    let too_old = v(50);
    assert!(
        claude_preflight_decision(
            false,
            true,
            too_old,
            settings_path(),
            claude_settings(),
            true,
            true,
            false,
        )
        .is_ok()
    );
    assert!(
        claude_preflight_decision(
            true,
            false,
            too_old,
            settings_path(),
            claude_settings(),
            true,
            true,
            false,
        )
        .is_ok()
    );
}

#[test]
fn claude_preflight_blocks_old_versions_and_disabled_settings() {
    assert_eq!(
        claude_decision(v(50), claude_settings(), false, false),
        Err(PreflightError::ClaudeTooOld {
            found: CliVersion::new(2, 1, 50)
        }),
    );

    let settings = claude_rc::ClaudeRcSettings {
        disable_remote_control: true,
        ..claude_settings()
    };
    assert_eq!(
        claude_decision(v(173), settings, false, false),
        Err(PreflightError::ClaudeRemoteControlDisabled {
            settings_path: settings_path()
        }),
    );
}

#[test]
fn claude_preflight_auth_conflict_gate_starts_at_2_1_157() {
    let settings = claude_rc::ClaudeRcSettings {
        api_key_helper: true,
        env_auth_conflict: true,
        ..claude_settings()
    };
    assert!(claude_decision(v(156), settings.clone(), true, true).is_ok());
    assert_eq!(
        claude_decision(v(157), settings, true, true),
        Err(PreflightError::ClaudeAuthConflict {
            sources: vec![
                ClaudeAuthConflictSource::ApiKeyEnv,
                ClaudeAuthConflictSource::AuthTokenEnv,
                ClaudeAuthConflictSource::ApiKeyHelperSetting,
                ClaudeAuthConflictSource::SettingsEnv,
            ],
        }),
    );
}

#[test]
fn claude_preflight_custom_endpoint_gate_starts_at_2_1_196() {
    let settings = claude_rc::ClaudeRcSettings {
        env_endpoint_conflict: true,
        ..claude_settings()
    };
    assert!(
        claude_preflight_decision(
            true,
            true,
            v(195),
            settings_path(),
            settings.clone(),
            false,
            false,
            true,
        )
        .is_ok()
    );
    assert_eq!(
        claude_preflight_decision(
            true,
            true,
            v(196),
            settings_path(),
            settings,
            false,
            false,
            true,
        ),
        Err(PreflightError::ClaudeAuthConflict {
            sources: vec![
                ClaudeAuthConflictSource::EndpointEnv,
                ClaudeAuthConflictSource::SettingsEndpoint,
            ],
        }),
    );
}

#[test]
fn claude_preflight_unknown_version_applies_only_settings_independent_gate() {
    let settings = claude_rc::ClaudeRcSettings {
        api_key_helper: true,
        ..claude_settings()
    };
    assert!(claude_decision(None, settings, true, false).is_ok());

    let settings = claude_rc::ClaudeRcSettings {
        disable_remote_control: true,
        ..claude_settings()
    };
    assert_eq!(
        claude_decision(None, settings, false, false),
        Err(PreflightError::ClaudeRemoteControlDisabled {
            settings_path: settings_path()
        }),
    );
}

#[test]
fn preflight_error_carries_the_official_install_command() {
    let msg = PreflightError::CodexStandaloneMissing.to_string();
    assert!(
        msg.contains(CODEX_INSTALL_COMMAND),
        "guidance names the installer"
    );
    assert!(
        msg.contains("[remote_control] codex"),
        "guidance names the toggle"
    );

    let msg = PreflightError::ClaudeTooOld {
        found: CliVersion::new(2, 1, 50),
    }
    .to_string();
    assert!(msg.contains("[remote_control] claude"));
    assert!(msg.contains(">= 2.1.51"));

    let msg = PreflightError::ClaudeAuthConflict {
        sources: vec![ClaudeAuthConflictSource::ApiKeyEnv],
    }
    .to_string();
    assert!(msg.contains("ANTHROPIC_API_KEY"));
    assert!(msg.contains("[remote_control] claude = false"));
}

#[test]
fn ensure_codex_daemon_requires_toggle_and_standalone() {
    // codex off → never ensure, install present or not.
    assert!(!should_ensure_codex_daemon(false, false));
    assert!(!should_ensure_codex_daemon(false, true));
    // codex on → ensure iff the managed standalone install is present.
    assert!(!should_ensure_codex_daemon(true, false));
    assert!(should_ensure_codex_daemon(true, true));
}

fn host(argv: &[&str]) -> HostPane {
    HostPane {
        argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: PathBuf::from("/repo"),
    }
}

fn daemon_view() -> DaemonView {
    DaemonView {
        name: VIEW_NAME.to_owned(),
        content: vec![
            host(&["rimz", "daemon", "content", "--slot", "0"]),
            host(&["rimz", "daemon", "content", "--slot", "1"]),
        ],
        hosts: vec![
            host(&["rimz", "codex", "app-server", "serve"]),
            host(&["claude", "remote-control", "--spawn", "worktree"]),
        ],
        loop_panel: host(&["rimz", "loop", "watch", "--hold"]),
    }
}

#[test]
fn missing_managed_panes_diffs_the_daemon_view_spec() {
    let present = [
        pane(
            Some("rimz daemon content --slot 0 --worktree-root /repo"),
            Some(VIEW_NAME),
        ),
        pane(Some("rimz codex app-server serve"), Some(VIEW_NAME)),
        pane(Some("rimz loop watch --hold"), Some(VIEW_NAME)),
        pane(Some("user shell"), Some(VIEW_NAME)),
        pane(Some("claude remote-control --spawn worktree"), Some("work")),
    ];

    let missing = missing_managed_panes(&daemon_view(), &present)
        .into_iter()
        .map(|host| host.argv.join(" "))
        .collect::<Vec<_>>();

    assert_eq!(
        missing,
        vec![
            "rimz daemon content --slot 1",
            "claude remote-control --spawn worktree"
        ]
    );
}

#[test]
fn missing_managed_panes_is_empty_when_every_managed_pane_is_present() {
    let present = [
        pane(
            Some("rimz daemon content --slot 0 --worktree-root /repo"),
            Some(VIEW_NAME),
        ),
        pane(
            Some("rimz daemon content --slot 1 --worktree-root /repo"),
            Some(VIEW_NAME),
        ),
        pane(Some("rimz codex app-server serve"), Some(VIEW_NAME)),
        pane(
            Some("claude remote-control --spawn worktree"),
            Some(VIEW_NAME),
        ),
        pane(Some("rimz loop watch --hold"), Some(VIEW_NAME)),
    ];

    assert!(missing_managed_panes(&daemon_view(), &present).is_empty());
}

#[test]
fn detects_both_hosts_by_full_command_line() {
    // Zellij reports the full command line. Claude spells the subcommand
    // `remote-control`; the broker spells `app-server`.
    assert!(pane_is_host(&pane(
        Some("claude remote-control --spawn worktree"),
        None,
    )));
    assert!(pane_is_host(&pane(
        Some("rimz codex app-server serve --workspace-id w"),
        None,
    )));
}

#[test]
fn detects_hosts_by_spawn_command() {
    let host = PaneRef {
        command: Some("claude".to_owned()),
        spawn_command: Some("claude remote-control --spawn worktree".to_owned()),
        ..pane(Some("claude"), None)
    };

    assert!(pane_is_host(&host));
}

#[test]
fn detects_host_by_view_name_when_command_is_a_bare_basename() {
    // tmux reports only the basename, but the window carries the view name,
    // so any pane in the rimzd view is a host regardless of its command.
    assert!(pane_is_host(&pane(Some("claude"), Some(VIEW_NAME))));
    assert!(pane_is_host(&pane(Some("rimz"), Some(VIEW_NAME))));
}

#[test]
fn a_plain_agent_is_not_the_host() {
    // A real coding session: bare basename, no rimzd view. A plain `codex`
    // agent pane must never be classified as a host.
    assert!(!pane_is_host(&pane(Some("claude"), Some("2"))));
    assert!(!pane_is_host(&pane(Some("codex"), Some("3"))));
    assert!(!pane_is_host(&pane(Some("zsh"), None)));
}
