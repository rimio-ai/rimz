//! Daemon view assembly and launch for room start.

use std::path::PathBuf;

use rimz::config::DaemonConfig;
use rimz::mux::{BackgroundViewLaunch, BackgroundViewOptions, MuxBackend, SidebarPaneOptions};

use super::RoomTarget;

pub(super) fn build_daemon_view(
    remote_control: &rimz::config::RemoteControlConfig,
    daemon: &DaemonConfig,
    workspace: &rimz::ResolvedWorkspace,
    mux_config: &rimz::config::MultiplexerConfig,
    room: &RoomTarget<'_>,
) -> Option<BackgroundViewOptions> {
    let rimz_bin = rimz::proc::rimz_exe();
    Some(build_daemon_view_options(
        remote_control,
        daemon,
        workspace,
        mux_config,
        room,
        rimz_bin,
        which::which("claude").is_ok(),
        which::which("codex").is_ok(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_daemon_view_options(
    remote_control: &rimz::config::RemoteControlConfig,
    daemon: &DaemonConfig,
    workspace: &rimz::ResolvedWorkspace,
    mux_config: &rimz::config::MultiplexerConfig,
    room: &RoomTarget<'_>,
    rimz_bin: PathBuf,
    claude_present: bool,
    codex_present: bool,
) -> BackgroundViewOptions {
    // The daemon view is born `sidebar | content | hosts…`, so it carries the
    // same global sidebar the working view runs (same session, workspace, and
    // `rimz` bin). The content column defaults to stats and keeps the view
    // useful even with no daemon host.
    let width_override = room.width_override();
    BackgroundViewOptions {
        view: rimz::daemon_view::daemon_view_spec(rimz::daemon_view::DaemonViewSpecParams {
            remote_control,
            daemon,
            rimz_bin: &rimz_bin,
            workspace_id: &workspace.workspace_id,
            session_name: &workspace.session_name,
            project_root: &workspace.project_root,
            worktree_root: &workspace.worktree_root,
            claude_present,
            codex_present,
        }),
        sidebar: SidebarPaneOptions {
            session_name: workspace.session_name.clone(),
            workspace_id: workspace.workspace_id.clone(),
            project_root: workspace.project_root.clone(),
            extra_env: room.extra_env.clone(),
            cwd: workspace.worktree_root.clone(),
            width: room.width,
            birth_size: room.birth_size(width_override),
            width_override,
            rimz_bin,
            replace_existing: false,
            pristine_birth: false,
            config: mux_config.clone(),
            resume_tabs: Vec::new(),
            refresh_ms: room.refresh_ms,
        },
    }
}

/// Ensure the per-user Codex remote-control daemon (a detached singleton keyed by
/// its control socket — never a pane) and open the `rimzd` daemon view,
/// best-effort. [`rimz::remote_control::ensure_codex_daemon`] self-gates on the
/// managed standalone install, so a missing install no-ops here just as
/// `rimz start` skips that host. On Zellij the view already leads from session
/// birth ([`MuxBackend::open_sidebar`] renders it first), so this is the
/// idempotent `AlreadyRunning` no-op there; on tmux it opens the window and
/// leads it via `swap-window`. The view always carries a content column (live
/// stats by default); daemon hosts are conditional.
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
            view = rimz::daemon_view::VIEW_NAME,
            "launched the daemon view",
        ),
        Ok(BackgroundViewLaunch::AlreadyRunning) => {
            tracing::debug!(
                session = %workspace.session_name,
                "daemon view already present; repairing missing managed panes",
            );
            rimz::daemon_view::repair_daemon_view(
                backend,
                &workspace.session_name,
                &workspace.workspace_id,
                &opts.view,
            );
        }
        Err(rimz::mux::MuxErr::SessionNotFound { session }) => tracing::debug!(
            session = %session,
            "daemon view deferred; session not addressable yet (pre-attach gate will rebirth it)",
        ),
        Err(err) => tracing::warn!(
            session = %workspace.session_name,
            error = %err,
            "daemon view launch failed; continuing without it",
        ),
    }
}
