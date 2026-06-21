use super::*;

pub(super) fn build_daemon_view(
    config: &rimz::config::RemoteControlConfig,
    workspace: &rimz::ResolvedWorkspace,
    mux_config: &rimz::config::MultiplexerConfig,
    room: &RoomTarget<'_>,
) -> Option<BackgroundViewOptions> {
    let rimz_bin = match std::env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!(
                session = %workspace.session_name,
                error = %err,
                "daemon view skipped because the current executable is unavailable",
            );
            return None;
        }
    };
    Some(build_daemon_view_options(
        config,
        workspace,
        mux_config,
        room,
        rimz_bin,
        which::which("claude").is_ok(),
        which::which("codex").is_ok(),
    ))
}

fn build_daemon_view_options(
    config: &rimz::config::RemoteControlConfig,
    workspace: &rimz::ResolvedWorkspace,
    mux_config: &rimz::config::MultiplexerConfig,
    room: &RoomTarget<'_>,
    rimz_bin: PathBuf,
    claude_present: bool,
    codex_present: bool,
) -> BackgroundViewOptions {
    let stats = stats_pane(&rimz_bin, &workspace.worktree_root);
    let hosts = daemon_hosts(
        config,
        claude_present,
        codex_present,
        &rimz_bin,
        &workspace.workspace_id,
        &workspace.session_name,
        &workspace.project_root,
        &workspace.worktree_root,
    );
    // The daemon view is born `sidebar | stats | hosts…`, so it carries the same
    // global sidebar the working view runs (same session, workspace, and `rimz`
    // bin). Stats keeps the view useful even with no daemon host.
    BackgroundViewOptions {
        view: DaemonView {
            name: rimz::remote_control::VIEW_NAME.to_owned(),
            stats,
            hosts,
        },
        sidebar: SidebarPaneOptions {
            session_name: workspace.session_name.clone(),
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            cwd: workspace.worktree_root.clone(),
            width: room.width,
            birth_size: room.birth_size(),
            rimz_bin,
            replace_existing: false,
            config: mux_config.clone(),
            resume_tabs: Vec::new(),
            refresh_ms: room.refresh_ms,
        },
    }
}

/// Ensure the per-user Codex remote-control daemon (a detached singleton keyed by
/// its control socket — never a pane; its standalone-install precondition is
/// enforced earlier by [`rimz::remote_control::preflight`]) and open the `rimzd`
/// daemon view, best-effort. On Zellij the view already leads from session birth
/// ([`MuxBackend::open_sidebar`] renders it first), so this is the idempotent
/// `AlreadyRunning` no-op there; on tmux it opens the window and leads it via
/// `swap-window`. The view always carries the live stats pane; daemon hosts are
/// conditional.
pub(super) fn maybe_launch_remote_control(
    backend: &dyn MuxBackend,
    workspace: &rimz::ResolvedWorkspace,
    config: &rimz::config::RemoteControlConfig,
    daemon_view: Option<&BackgroundViewOptions>,
) {
    rimz::remote_control::ensure_codex_daemon(config);

    let Some(opts) = daemon_view else {
        return;
    };
    match backend.open_background_view(opts) {
        Ok(BackgroundViewLaunch::Launched) => tracing::info!(
            session = %workspace.session_name,
            view = rimz::remote_control::VIEW_NAME,
            "launched the daemon view",
        ),
        Ok(BackgroundViewLaunch::AlreadyRunning) => tracing::debug!(
            session = %workspace.session_name,
            "daemon view already present; skipping",
        ),
        Err(err) => tracing::warn!(
            session = %workspace.session_name,
            error = %err,
            "daemon view launch failed; continuing without it",
        ),
    }
}

/// The daemon host panes for the [`rimz::remote_control::VIEW_NAME`] view, in
/// display order (the first takes focus) — split out pure for testing. The Claude
/// remote-control host leads when its toggle is on *and* `claude` is on PATH (the
/// interactive host); the local Codex app-server broker follows whenever `codex`
/// is on PATH (ungated — it links no account, only reads). Live stats is a
/// separate always-present pane, not a daemon host.
#[allow(clippy::too_many_arguments)]
fn daemon_hosts(
    config: &rimz::config::RemoteControlConfig,
    claude_present: bool,
    codex_present: bool,
    rimz_bin: &Path,
    workspace_id: &rimz::WorkspaceId,
    session_name: &str,
    project_root: &Path,
    worktree_root: &Path,
) -> Vec<HostPane> {
    let mut hosts = Vec::new();
    if config.claude && claude_present {
        hosts.push(HostPane {
            argv: rimz::remote_control::claude_host_argv(),
            cwd: project_root.to_path_buf(),
        });
    }
    if codex_present {
        hosts.push(HostPane {
            argv: vec![
                rimz_bin.to_string_lossy().into_owned(),
                "codex".to_owned(),
                "app-server".to_owned(),
                "serve".to_owned(),
                "--workspace-id".to_owned(),
                workspace_id.as_str().to_owned(),
                "--session-name".to_owned(),
                session_name.to_owned(),
            ],
            cwd: worktree_root.to_path_buf(),
        });
    }
    hosts
}

fn stats_pane(rimz_bin: &Path, worktree_root: &Path) -> HostPane {
    HostPane {
        argv: vec![
            rimz_bin.to_string_lossy().into_owned(),
            "stats".to_owned(),
            "--refresh".to_owned(),
        ],
        cwd: worktree_root.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_hosts_orders_claude_then_the_ungated_broker() {
        use rimz::config::RemoteControlConfig;
        use rimz::ids::WorkspaceId;

        let rimz_bin = Path::new("/usr/bin/rimz");
        let wid = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
        let project = Path::new("/proj");
        let worktree = Path::new("/proj/wt");
        let hosts = |config: &RemoteControlConfig, claude: bool, codex: bool| {
            daemon_hosts(
                config,
                claude,
                codex,
                rimz_bin,
                &wid,
                "rimz-demo",
                project,
                worktree,
            )
        };

        assert!(hosts(&RemoteControlConfig::default(), true, false).is_empty());

        let codex = hosts(&RemoteControlConfig::default(), false, true);
        assert_eq!(codex.len(), 1);
        assert_eq!(codex[0].argv[0], "/usr/bin/rimz");
        assert!(codex[0].argv.iter().any(|arg| arg == "app-server"));
        assert_eq!(codex[0].cwd.as_path(), worktree);

        let claude_only = RemoteControlConfig {
            claude: true,
            codex: false,
        };
        assert!(hosts(&claude_only, false, false).is_empty());
        let claude = hosts(&claude_only, true, false);
        assert_eq!(claude.len(), 1);
        assert_eq!(claude[0].argv[0], "env");
        assert_eq!(
            claude[0].argv,
            rimz::remote_control::claude_host_argv(),
            "the daemon host unsets the pane-only Claude agent-view pin"
        );
        assert_eq!(claude[0].cwd.as_path(), project);

        let both = RemoteControlConfig {
            claude: true,
            codex: true,
        };
        let pair = hosts(&both, true, true);
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0].argv[0], "env");
        assert_eq!(pair[1].argv[0], "/usr/bin/rimz");
        assert!(pair[1].argv.iter().any(|arg| arg == "app-server"));
    }

    #[test]
    fn stats_pane_runs_refreshing_stats_from_worktree() {
        let rimz_bin = Path::new("/usr/bin/rimz");
        let worktree = Path::new("/proj/wt");
        let pane = stats_pane(rimz_bin, worktree);
        assert_eq!(
            pane.argv,
            vec![
                rimz_bin.to_string_lossy().into_owned(),
                "stats".to_owned(),
                "--refresh".to_owned(),
            ]
        );
        assert_eq!(pane.cwd.as_path(), worktree);
    }

    #[test]
    fn daemon_view_options_keep_stats_when_hosts_are_empty() {
        use rimz::config::{MultiplexerConfig, RemoteControlConfig};
        use rimz::ids::WorkspaceId;
        use rimz::mux::SidebarWidth;
        use rimz::workspace::RootClass;

        let wid = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
        let workspace = rimz::ResolvedWorkspace {
            workspace_id: wid.clone(),
            project_root: PathBuf::from("/proj"),
            root_class: RootClass::Repo,
            worktree_root: PathBuf::from("/proj/wt"),
            worktree_branch: None,
            session_name: "rimz-demo".to_owned(),
            mux_hint: None,
        };
        let mux_config = MultiplexerConfig::default();
        let width = SidebarWidth::default();
        let room = RoomTarget {
            workspace_id: &wid,
            project_root: Path::new("/proj"),
            session_name: "rimz-demo",
            cwd: Path::new("/proj/wt"),
            mux_config: &mux_config,
            width,
            detected_size: Some((120, 40)),
            refresh_ms: None,
        };
        let opts = build_daemon_view_options(
            &RemoteControlConfig::default(),
            &workspace,
            &mux_config,
            &room,
            PathBuf::from("/usr/bin/rimz"),
            false,
            false,
        );
        assert_eq!(opts.view.name, rimz::remote_control::VIEW_NAME);
        assert!(opts.view.hosts.is_empty());
        assert_eq!(
            opts.view.stats.argv,
            vec![
                "/usr/bin/rimz".to_owned(),
                "stats".to_owned(),
                "--refresh".to_owned(),
            ]
        );
        assert_eq!(opts.view.stats.cwd, PathBuf::from("/proj/wt"));
        assert_eq!(opts.sidebar.birth_size, width.birth_size(Some(120)));
    }
}
