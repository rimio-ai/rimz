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
    let hosts = background_view_hosts(
        config,
        which::which("claude").is_ok(),
        which::which("codex").is_ok(),
        &rimz_bin,
        &workspace.workspace_id,
        &workspace.session_name,
        &workspace.project_root,
        &workspace.worktree_root,
    );
    if hosts.is_empty() {
        return None;
    }
    // The daemon view is born `sidebar | hosts…`, so it carries the same global
    // sidebar the working view runs (same session, workspace, and `rimz` bin).
    Some(BackgroundViewOptions {
        name: rimz::remote_control::VIEW_NAME.to_owned(),
        hosts,
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
            resume_panes: Vec::new(),
            refresh_ms: room.refresh_ms,
        },
    })
}

/// Ensure the per-user Codex remote-control daemon (a detached singleton keyed by
/// its control socket — never a pane; its standalone-install precondition is
/// enforced earlier by [`rimz::remote_control::preflight`]) and open the `rimzd`
/// daemon view, best-effort. On Zellij the view already leads from session birth
/// ([`MuxBackend::open_sidebar`] renders it first), so this is the idempotent
/// `AlreadyRunning` no-op there; on tmux it opens the window and leads it via
/// `swap-window`. Skipped when there is no host pane.
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

/// The host panes for the [`rimz::remote_control::VIEW_NAME`] daemon view, in
/// display order (the first takes focus) — split out pure for testing. The Claude
/// remote-control host leads when its toggle is on *and* `claude` is on PATH (the
/// interactive host); the local Codex app-server broker follows whenever `codex`
/// is on PATH (ungated — it links no account, only reads). Empty when neither
/// applies, so the caller opens no view.
#[allow(clippy::too_many_arguments)]
fn background_view_hosts(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_view_hosts_orders_claude_then_the_ungated_broker() {
        use rimz::config::RemoteControlConfig;
        use rimz::ids::WorkspaceId;

        let rimz_bin = Path::new("/usr/bin/rimz");
        let wid = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("valid id");
        let project = Path::new("/proj");
        let worktree = Path::new("/proj/wt");
        let hosts = |config: &RemoteControlConfig, claude: bool, codex: bool| {
            background_view_hosts(
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
}
