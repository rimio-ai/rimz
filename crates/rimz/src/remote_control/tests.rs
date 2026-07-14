use super::*;
use crate::ids::{MuxName, PaneId};

fn pane(command: Option<&str>, view_name: Option<&str>) -> PaneRef {
    pane_with_id("%1", command, view_name)
}

fn pane_with_id(raw: &str, command: Option<&str>, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        pane_id: PaneId::from_parts(MuxName::Tmux, raw),
        session_name: "rimz-demo".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: view_name.map(ToOwned::to_owned),
        title: None,
        is_focused: false,
        is_floating: false,
        command: command.map(ToOwned::to_owned),
        foreground_cmdline: None,
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

fn spawned_pane(command: &str, spawn_command: &str, view_name: Option<&str>) -> PaneRef {
    PaneRef {
        spawn_command: Some(spawn_command.to_owned()),
        ..pane(Some(command), view_name)
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

#[test]
fn codex_toggle_uses_symmetric_start_and_stop_commands() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    assert_eq!(
        codex_command(bin),
        vec![
            bin.display().to_string(),
            "remote-control".to_owned(),
            "start".to_owned(),
        ]
    );
    assert_eq!(
        codex_stop_command(bin),
        vec![
            bin.display().to_string(),
            "remote-control".to_owned(),
            "stop".to_owned(),
        ]
    );
    assert_eq!(codex_daemon_action(true), "start");
    assert_eq!(codex_daemon_action(false), "stop");
}

#[test]
fn codex_daemon_commands_anchor_descendants_to_codex_home() {
    let bin = Path::new("/home/u/.codex/packages/standalone/current/codex");
    let home = Path::new("/home/u/.codex");
    for argv in [codex_command(bin), codex_stop_command(bin)] {
        let spec = codex_daemon_command_spec(&argv, home).expect("non-empty Codex command");
        assert_eq!(spec.cwd.as_deref(), Some(home));
        assert_eq!(spec.program, argv[0]);
        assert_eq!(spec.args, argv[1..]);
    }
}

#[cfg(unix)]
fn daemon_pid_record(pid: u32) -> CodexDaemonPidRecord {
    CodexDaemonPidRecord {
        pid,
        process_start_time: "Mon Jul 13 02:03:45 2026".to_owned(),
    }
}

#[cfg(unix)]
fn stale_daemon_snapshot(codex_home: &Path) -> CodexDaemonProcessSnapshot {
    let uid = crate::proc::own_uid().expect("unix uid");
    let updater = codex_home.join("packages/standalone/releases/0.144.3/bin/codex");
    CodexDaemonProcessSnapshot {
        app_state: 'Z',
        app_parent: 41,
        app_uid: uid,
        app_identity_matches: true,
        updater_state: 'S',
        updater_uid: uid,
        updater_identity_matches: true,
        updater_exe: updater.clone(),
        updater_argv: [
            updater.into_os_string(),
            OsString::from("app-server"),
            OsString::from("daemon"),
            OsString::from("pid-update-loop"),
        ]
        .into(),
        updater_children: vec![42],
    }
}

#[cfg(unix)]
#[test]
fn stale_codex_recovery_requires_the_exact_owned_zombie_tree() {
    let home = Path::new("/home/u/.codex");
    let app = daemon_pid_record(42);
    let updater = daemon_pid_record(41);
    let snapshot = stale_daemon_snapshot(home);
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &snapshot),
        Some(41),
    );

    let mut live_app = stale_daemon_snapshot(home);
    live_app.app_state = 'S';
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &live_app),
        None,
    );

    let mut reused_app_pid = stale_daemon_snapshot(home);
    reused_app_pid.app_identity_matches = false;
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &reused_app_pid),
        None,
    );

    let mut wrong_owner = stale_daemon_snapshot(home);
    wrong_owner.updater_uid = wrong_owner.updater_uid.saturating_add(1);
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &wrong_owner),
        None,
    );

    let mut reused_updater_pid = stale_daemon_snapshot(home);
    reused_updater_pid.updater_identity_matches = false;
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &reused_updater_pid),
        None,
    );

    let mut unrelated_parent = stale_daemon_snapshot(home);
    unrelated_parent.app_parent = 7;
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &unrelated_parent),
        None,
    );

    let mut extra_child = stale_daemon_snapshot(home);
    extra_child.updater_children.push(43);
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &extra_child),
        None,
    );

    let mut unrelated_executable = stale_daemon_snapshot(home);
    unrelated_executable.updater_exe = PathBuf::from("/tmp/codex");
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &unrelated_executable),
        None,
    );

    let mut unrelated_argv = stale_daemon_snapshot(home);
    unrelated_argv.updater_argv[3] = OsString::from("something-else");
    assert_eq!(
        stale_codex_updater_pid(home, &app, &updater, &unrelated_argv),
        None,
    );
}

#[test]
fn codex_daemon_pid_record_requires_the_upstream_identity_fields() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("app-server.pid");
    std::fs::write(
        &path,
        r#"{"pid":42,"processStartTime":"Mon Jul 13 02:08:48 2026"}"#,
    )
    .expect("write valid record");
    let record = read_codex_daemon_pid_record(&path).expect("valid record");
    assert_eq!(record.pid, 42);

    std::fs::write(&path, r#"{"pid":42}"#).expect("write incomplete record");
    assert!(read_codex_daemon_pid_record(&path).is_none());

    std::fs::write(
        &path,
        r#"{"pid":0,"processStartTime":"Mon Jul 13 02:08:48 2026"}"#,
    )
    .expect("write zero pid record");
    assert!(read_codex_daemon_pid_record(&path).is_none());
}

#[test]
fn daemon_view_spec_orders_the_ungated_broker_then_claude() {
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
    let rimz_bin = Path::new("/usr/bin/rimz");
    let project_root = Path::new("/proj");
    let worktree_root = Path::new("/proj/wt");
    let spec = |remote_control: &RemoteControlConfig, claude_present, codex_present| {
        daemon_view_spec(DaemonViewSpecParams {
            remote_control,
            daemon: &DaemonConfig::default(),
            rimz_bin,
            workspace_id: &workspace_id,
            session_name: "rimz-demo",
            project_root,
            worktree_root,
            claude_present,
            codex_present,
        })
    };

    assert!(
        spec(&RemoteControlConfig::default(), true, false)
            .hosts
            .is_empty()
    );
    let codex = spec(&RemoteControlConfig::default(), false, true);
    assert_eq!(codex.hosts.len(), 1);
    assert_eq!(codex.hosts[0].argv[0], "/usr/bin/rimz");
    assert!(codex.hosts[0].argv.iter().any(|arg| arg == "app-server"));
    assert_eq!(codex.hosts[0].cwd, worktree_root);

    let claude_only = RemoteControlConfig {
        claude: true,
        codex: false,
    };
    assert!(spec(&claude_only, false, false).hosts.is_empty());
    let claude = spec(&claude_only, true, false);
    assert_eq!(claude.hosts.len(), 1);
    assert_eq!(claude.hosts[0].argv, claude_host_argv());
    assert_eq!(claude.hosts[0].cwd, project_root);

    let both = RemoteControlConfig {
        claude: true,
        codex: true,
    };
    let pair = spec(&both, true, true);
    assert_eq!(pair.hosts.len(), 2);
    assert!(pair.hosts[0].argv.iter().any(|arg| arg == "app-server"));
    assert_eq!(pair.hosts[1].argv[0], "claude");
}

#[test]
fn daemon_view_spec_keeps_content_and_loop_panel_without_hosts() {
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
    let view = daemon_view_spec(DaemonViewSpecParams {
        remote_control: &RemoteControlConfig::default(),
        daemon: &DaemonConfig::default(),
        rimz_bin: Path::new("/usr/bin/rimz"),
        workspace_id: &workspace_id,
        session_name: "rimz-demo",
        project_root: Path::new("/proj"),
        worktree_root: Path::new("/proj/wt"),
        claude_present: false,
        codex_present: false,
    });

    assert_eq!(view.name, VIEW_NAME);
    assert!(view.hosts.is_empty());
    assert_eq!(
        view.content,
        vec![content_supervisor_pane(
            0,
            Path::new("/usr/bin/rimz"),
            Path::new("/proj/wt")
        )]
    );
    assert_eq!(
        view.loop_panel,
        loop_panel(Path::new("/usr/bin/rimz"), Path::new("/proj/wt"))
    );
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
fn managed_pane_reconciliation_diffs_the_daemon_view_spec() {
    let present = [
        spawned_pane(
            "rimz",
            "rimz daemon content --slot 0 --worktree-root /repo",
            Some(VIEW_NAME),
        ),
        spawned_pane("rimz", "rimz codex app-server serve", Some(VIEW_NAME)),
        spawned_pane("rimz", "rimz loop watch --hold", Some(VIEW_NAME)),
        pane(Some("user shell"), Some(VIEW_NAME)),
        spawned_pane(
            "claude",
            "claude remote-control --spawn worktree",
            Some("work"),
        ),
    ];

    let missing = managed_pane_reconciliation(&daemon_view(), &present)
        .spawn
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
fn managed_pane_reconciliation_is_empty_when_every_managed_pane_is_present() {
    let present = [
        spawned_pane(
            "rimz",
            "rimz daemon content --slot 0 --worktree-root /repo",
            Some(VIEW_NAME),
        ),
        spawned_pane(
            "rimz",
            "rimz daemon content --slot 1 --worktree-root /repo",
            Some(VIEW_NAME),
        ),
        spawned_pane("rimz", "rimz codex app-server serve", Some(VIEW_NAME)),
        spawned_pane(
            "claude",
            "claude remote-control --spawn worktree",
            Some(VIEW_NAME),
        ),
        spawned_pane("rimz", "rimz loop watch --hold", Some(VIEW_NAME)),
    ];

    let reconciliation = managed_pane_reconciliation(&daemon_view(), &present);
    assert!(reconciliation.spawn.is_empty());
    assert!(reconciliation.close.is_empty());
}

#[test]
fn title_identity_prevents_respawn_after_foreground_command_churn() {
    let mut daemon_host = pane_with_id(
        "%9",
        Some(
            "/home/u/.local/share/claude/versions/2.1.209 --print --sdk-url wss://example --session-id cse_123",
        ),
        Some(VIEW_NAME),
    );
    daemon_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let reconciliation = managed_pane_reconciliation(&daemon_view(), &[daemon_host]);

    assert!(
        !reconciliation
            .spawn
            .iter()
            .any(|host| { host_marker(host) == Some(ManagedPaneMarker::ClaudeRemoteControl) })
    );
}

#[test]
fn title_identity_matches_content_supervisor_after_child_churn() {
    let mut content = pane_with_id("%8", Some("rimz stats --refresh --hold"), Some(VIEW_NAME));
    content.title = Some("rimz daemon content --slot 0".to_owned());
    let reconciliation = managed_pane_reconciliation(&daemon_view(), &[content]);

    assert!(
        !reconciliation
            .spawn
            .iter()
            .any(|host| { host_marker(host) == Some(ManagedPaneMarker::ContentSlot(0)) })
    );
}

#[test]
fn reconciliation_closes_surplus_managed_panes_but_keeps_the_oldest() {
    let panes = [
        spawned_pane(
            "claude",
            "claude remote-control --spawn worktree",
            Some(VIEW_NAME),
        ),
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%7"),
            ..spawned_pane(
                "claude",
                "claude remote-control --spawn worktree",
                Some(VIEW_NAME),
            )
        },
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, "%3"),
            ..spawned_pane(
                "claude",
                "claude remote-control --spawn worktree",
                Some(VIEW_NAME),
            )
        },
    ];

    assert_eq!(
        managed_pane_reconciliation(&daemon_view(), &panes).close,
        vec![
            PaneId::from_parts(MuxName::Tmux, "%3"),
            PaneId::from_parts(MuxName::Tmux, "%7"),
        ]
    );
}

#[test]
fn disabled_claude_host_selection_uses_title_and_stays_in_the_daemon_view() {
    let mut daemon_host = pane_with_id("%4", Some("claude-sdk"), Some(VIEW_NAME));
    daemon_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let daemon_host_id = daemon_host.pane_id.clone();
    let mut working_host = pane_with_id("%5", Some("claude-sdk"), Some("work"));
    working_host.title = Some("claude remote-control --spawn worktree".to_owned());
    let user_pane = spawned_pane("nvim", "nvim remote-control.md", Some(VIEW_NAME));
    let panes = [daemon_host, working_host, user_pane];

    let mut disabled = daemon_view();
    disabled
        .hosts
        .retain(|host| host_marker(host) != Some(ManagedPaneMarker::ClaudeRemoteControl));
    assert_eq!(
        managed_pane_reconciliation(&disabled, &panes).close,
        vec![daemon_host_id]
    );
    assert!(
        managed_pane_reconciliation(&daemon_view(), &panes)
            .close
            .is_empty()
    );
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

#[test]
fn claude_host_presence_requires_the_managed_pane_marker() {
    let managed = spawned_pane(
        "claude",
        "claude remote-control --spawn worktree",
        Some(VIEW_NAME),
    );
    let user_session = spawned_pane("claude", "claude", Some("work"));
    let mut managed_after_churn = pane(Some("claude-sdk"), Some(VIEW_NAME));
    managed_after_churn.title = Some("claude remote-control --spawn worktree".to_owned());

    assert!(claude_host_present(&[managed, user_session.clone()]));
    assert!(claude_host_present(&[managed_after_churn]));
    assert!(!claude_host_present(&[user_session]));
}
