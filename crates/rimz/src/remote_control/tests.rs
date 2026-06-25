use super::*;
use crate::ids::{MuxName, PaneId};
use std::collections::BTreeMap;

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
fn claude_host_argv_unsets_the_agent_view_pin() {
    let argv = claude_host_argv();
    assert_eq!(
        &argv[..4],
        ["env", "-u", claude_rc::DISABLE_AGENT_VIEW_ENV, "claude"]
    );
    assert_eq!(&argv[4..], ["remote-control", "--spawn", "worktree"]);
    assert!(
        !argv.iter().any(|arg| arg.contains("=1")),
        "the host argv must not set the pane-only agent-view kill switch"
    );
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
        PreflightError::ClaudeAgentViewDisabled {
            settings_path: settings_path(),
            found: CliVersion::new(2, 1, 173),
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
            true
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
            true
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
fn claude_preflight_agent_view_gate_starts_at_2_1_173() {
    let settings = claude_rc::ClaudeRcSettings {
        disable_agent_view: true,
        ..claude_settings()
    };
    assert!(claude_decision(v(172), settings.clone(), false, false).is_ok());
    assert_eq!(
        claude_decision(v(173), settings, false, false),
        Err(PreflightError::ClaudeAgentViewDisabled {
            settings_path: settings_path(),
            found: CliVersion::new(2, 1, 173),
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
fn claude_preflight_unknown_version_applies_only_settings_independent_gate() {
    let settings = claude_rc::ClaudeRcSettings {
        disable_agent_view: true,
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
fn elevation_wrapper_gate_reads_the_entrypoint() {
    assert!(command_starts_with_elevation_wrapper("sudo su"));
    assert!(command_starts_with_elevation_wrapper(
        "/usr/bin/doas claude"
    ));
    assert!(command_starts_with_elevation_wrapper("su -"));
    assert!(!command_starts_with_elevation_wrapper("claude"));
    assert!(!command_starts_with_elevation_wrapper("zsh"));
}

#[test]
fn elevated_agent_scan_detects_foreign_uid_descendant_past_sudo_su() {
    let fixture = ProcFixture::new([
        ProcNode::new(10, 1_000, "zsh", &[20]),
        ProcNode::new(20, 1_000, "sudo su", &[21]),
        ProcNode::new(21, 0, "-bash", &[22]),
        ProcNode::new(
            22,
            0,
            "node /opt/node_modules/@anthropic-ai/claude-code/cli.js",
            &[],
        )
        .with_comm("node"),
    ]);

    let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

    assert_eq!(elevated.kind.as_str(), "claude");
    assert_eq!(elevated.uid, 0);
}

#[test]
fn elevated_agent_scan_can_fall_back_to_precise_comm() {
    let fixture = ProcFixture::new([
        ProcNode::new(10, 1_000, "zsh", &[20]),
        ProcNode::new(20, 1_000, "sudo su", &[21]),
        ProcNode::new(21, 0, "-bash", &[22]),
        ProcNode::new(22, 0, "", &[]).with_comm("claude"),
    ]);

    let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

    assert_eq!(elevated.kind.as_str(), "claude");
    assert_eq!(elevated.uid, 0);
}

#[test]
fn elevated_agent_scan_detects_direct_sudo_agent() {
    let fixture = ProcFixture::new([
        ProcNode::new(10, 1_000, "zsh", &[20]),
        ProcNode::new(20, 0, "sudo -u root claude", &[]),
    ]);

    let elevated = fixture.elevated_agent(10, 1_000).expect("foreign agent");

    assert_eq!(elevated.kind.as_str(), "claude");
    assert_eq!(elevated.uid, 0);
}

#[test]
fn elevated_agent_scan_ignores_same_uid_and_non_wrapper_paths() {
    let same_uid = ProcFixture::new([
        ProcNode::new(10, 1_000, "zsh", &[20]),
        ProcNode::new(20, 1_000, "sudo su", &[21]),
        ProcNode::new(21, 1_000, "claude", &[]),
    ]);
    assert_eq!(same_uid.elevated_agent(10, 1_000), None);

    let no_wrapper = ProcFixture::new([
        ProcNode::new(10, 1_000, "zsh", &[20]),
        ProcNode::new(20, 0, "claude", &[]),
    ]);
    assert_eq!(no_wrapper.elevated_agent(10, 1_000), None);
}

#[test]
fn codex_daemon_cmdline_matches_the_app_server_surface() {
    // The per-user daemon runs the codex binary on its daemon surface.
    assert!(is_codex_daemon_cmdline(
        "/home/u/.codex/packages/standalone/current/codex app-server"
    ));
    assert!(is_codex_daemon_cmdline("codex remote-control start"));
}

#[test]
fn codex_daemon_cmdline_rejects_a_plain_session_or_other_server() {
    // A plain in-pane codex TUI is a standalone session, not the daemon —
    // process liveness reaps it, so it must not join the daemon set.
    assert!(!is_codex_daemon_cmdline("codex"));
    assert!(!is_codex_daemon_cmdline("codex --model gpt-5.5"));
    // A non-codex server that merely spells a marker is not the codex daemon.
    assert!(!is_codex_daemon_cmdline("some-other app-server"));
}

#[test]
fn codex_cli_cmdline_matches_bare_cli_not_daemon() {
    // The in-pane TUI a user launches, including the npm `node` wrapper.
    assert!(is_codex_cli_cmdline("codex"));
    assert!(is_codex_cli_cmdline("codex --model gpt-5.5"));
    assert!(is_codex_cli_cmdline("node /usr/bin/codex"));
    // The daemon, the remote-control host, and Rimz's broker all spell a
    // daemon surface, so none reads as the in-pane CLI.
    assert!(!is_codex_cli_cmdline("codex app-server"));
    assert!(!is_codex_cli_cmdline("codex remote-control start"));
    assert!(!is_codex_cli_cmdline(
        "rimz codex app-server serve --workspace-id w"
    ));
    // A non-codex process is never the codex CLI.
    assert!(!is_codex_cli_cmdline("zsh"));
}

#[test]
fn codex_resume_cmdline_yields_session_id() {
    assert_eq!(
        codex_resumed_session_id_from_cmdline("codex resume 019ea276").as_deref(),
        Some("019ea276")
    );
    assert_eq!(
        codex_resumed_session_id_from_cmdline("node /usr/bin/codex resume sess-2").as_deref(),
        Some("sess-2")
    );
    assert_eq!(
        codex_resumed_session_id_from_cmdline("codex --model gpt-5 resume sess"),
        None
    );
    assert_eq!(
        codex_resumed_session_id_from_cmdline("codex app-server resume sess"),
        None
    );
}

#[test]
fn codex_resume_root_yields_session_id_from_root_or_single_child() {
    assert_eq!(
        codex_resumed_session_id_for_root_with(
            200,
            &|pid| (pid == 200).then_some("codex resume root-sess".to_owned()),
            &|_| Vec::new(),
        )
        .as_deref(),
        Some("root-sess")
    );
    assert_eq!(
        codex_resumed_session_id_for_root_with(
            200,
            &|pid| match pid {
                200 => Some("zsh".to_owned()),
                300 => Some("codex resume child-sess".to_owned()),
                _ => None,
            },
            &|pid| (pid == 200).then_some(vec![300]).unwrap_or_default(),
        )
        .as_deref(),
        Some("child-sess")
    );
    assert_eq!(
        codex_resumed_session_id_for_root_with(
            200,
            &|pid| match pid {
                300 => Some("codex resume child-a".to_owned()),
                301 => Some("codex resume child-b".to_owned()),
                _ => Some("zsh".to_owned()),
            },
            &|pid| (pid == 200).then_some(vec![300, 301]).unwrap_or_default(),
        ),
        None
    );
}

struct ProcNode {
    pid: u32,
    uid: u32,
    comm: Option<&'static str>,
    cmdline: &'static str,
    children: &'static [u32],
}

impl ProcNode {
    const fn new(pid: u32, uid: u32, cmdline: &'static str, children: &'static [u32]) -> Self {
        Self {
            pid,
            uid,
            comm: None,
            cmdline,
            children,
        }
    }

    const fn with_comm(mut self, comm: &'static str) -> Self {
        self.comm = Some(comm);
        self
    }
}

struct ProcFixture {
    nodes: BTreeMap<u32, ProcNode>,
}

impl ProcFixture {
    fn new(nodes: impl IntoIterator<Item = ProcNode>) -> Self {
        Self {
            nodes: nodes.into_iter().map(|node| (node.pid, node)).collect(),
        }
    }

    fn elevated_agent(&self, root: u32, own_uid: u32) -> Option<ElevatedAgent> {
        elevated_in_pane_agent_with(
            root,
            own_uid,
            &|pid| {
                self.nodes
                    .get(&pid)
                    .map(|node| node.children.to_vec())
                    .unwrap_or_default()
            },
            &|pid| self.nodes.get(&pid).map(|node| node.cmdline.to_owned()),
            &|pid| {
                self.nodes
                    .get(&pid)
                    .and_then(|node| node.comm.map(str::to_owned))
            },
            &|pid| self.nodes.get(&pid).map(|node| node.uid),
        )
    }
}
