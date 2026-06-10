//! tmux room options and sidebar-pane classification.

use crate::config::TmuxConfig;
use crate::feed::PaneRef;
use crate::mux::{SidebarPaneOptions, ViewSidebars};

/// Pane title the sidebar renderer sets through the terminal title escape. The
/// host binary is now `rimz`, so tmux identifies chrome through this title
/// instead of the foreground command name.
pub(super) const SIDEBAR_PANE_TITLE: &str = "rimz-sidebar";

/// The `rimz sidebar serve …` argv a tmux sidebar pane runs. Shared by initial
/// launch and in-place recovery so the two cannot drift.
pub(super) fn sidebar_serve_command(opts: &SidebarPaneOptions) -> Vec<String> {
    let mut command = vec![
        opts.rimz_bin.to_string_lossy().into_owned(),
        "sidebar".to_owned(),
        "serve".to_owned(),
        "--mux".to_owned(),
        "tmux".to_owned(),
        "--workspace-id".to_owned(),
        opts.workspace_id.as_str().to_owned(),
        "--session-name".to_owned(),
        opts.session_name.clone(),
    ];
    if let Some(refresh_ms) = opts.refresh_ms {
        command.extend(["--refresh-ms".to_owned(), refresh_ms.to_string()]);
    }
    command
}

pub(super) fn is_tmux_sidebar(pane: &PaneRef) -> bool {
    pane.command.as_deref() == Some(SIDEBAR_PANE_TITLE)
}

/// Group a pane list into per-window [`ViewSidebars`] for the reconcile planner:
/// each window's sidebar panes and whether it holds a user-working pane. Managed
/// daemon hosts in `rimzd` are not work. Panes with no window id are skipped.
/// First-seen window order.
pub(super) fn tmux_views_with_sidebars(panes: &[PaneRef]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for pane in panes {
        let Some(view) = pane.view_id.as_deref() else {
            continue;
        };
        let slot = *index.entry(view.to_owned()).or_insert_with(|| {
            views.push(ViewSidebars {
                view: view.to_owned(),
                sidebar_panes: Vec::new(),
                has_working: false,
                has_daemon_host: false,
            });
            views.len() - 1
        });
        if is_tmux_sidebar(pane) {
            views[slot].sidebar_panes.push(pane.pane_id.clone());
        } else if crate::remote_control::pane_is_host(pane) {
            views[slot].has_daemon_host = true;
        } else {
            views[slot].has_working = true;
        }
    }
    views
}

pub(super) fn tmux_bool(value: bool) -> String {
    if value { "on" } else { "off" }.to_owned()
}

pub(super) fn tmux_server_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("focus-events", tmux_bool(config.focus_events)),
        ("set-clipboard", config.set_clipboard.as_str().to_owned()),
        ("extended-keys", tmux_bool(config.extended_keys)),
        (
            "extended-keys-format",
            config.extended_keys_format.as_str().to_owned(),
        ),
        ("escape-time", config.escape_time_ms.to_string()),
    ]
}

pub(super) fn tmux_session_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("mouse", tmux_bool(config.mouse)),
        ("history-limit", config.history_limit.to_string()),
        ("renumber-windows", tmux_bool(config.renumber_windows)),
    ]
}

pub(super) fn tmux_window_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("allow-passthrough", tmux_bool(config.allow_passthrough)),
        ("aggressive-resize", tmux_bool(config.aggressive_resize)),
        (
            "pane-border-status",
            config.pane_border_status.as_str().to_owned(),
        ),
        (
            "pane-border-lines",
            config.pane_border_lines.as_str().to_owned(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::MultiplexerConfig;
    use crate::feed::PaneRef;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use crate::mux::{SidebarPaneOptions, SidebarWidth};

    fn sidebar_opts(refresh_ms: Option<u16>) -> SidebarPaneOptions {
        let width = SidebarWidth::default();
        SidebarPaneOptions {
            session_name: "room".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-tmux-refresh")),
            project_root: PathBuf::from("/tmp/rimz-tmux-refresh"),
            cwd: PathBuf::from("/tmp/rimz-tmux-refresh"),
            width,
            birth_size: width.birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: MultiplexerConfig::default(),
            resume_panes: Vec::new(),
            refresh_ms,
        }
    }

    fn tmux_pane(id: &str, view: &str, command: &str) -> PaneRef {
        PaneRef {
            pane_id: PaneId::from_parts(MuxName::Tmux, id),
            session_name: "room".to_owned(),
            view_id: Some(view.to_owned()),
            view_kind: None,
            view_name: None,
            is_focused: false,
            command: Some(command.to_owned()),
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
    fn sidebar_command_threads_refresh_override() {
        let command = sidebar_serve_command(&sidebar_opts(Some(75)));
        assert_eq!(
            &command[command.len() - 2..],
            ["--refresh-ms".to_owned(), "75".to_owned()],
        );

        let without = sidebar_serve_command(&sidebar_opts(None));
        assert!(
            !without.iter().any(|arg| arg == "--refresh-ms"),
            "default launch leaves refresh cadence config-driven: {without:?}",
        );
    }

    #[test]
    fn views_with_sidebars_classifies_working_orphan_and_daemon_windows() {
        let mut host = tmux_pane("%5", "@2", "rimz");
        host.view_name = Some(crate::remote_control::VIEW_NAME.to_owned());
        let panes = vec![
            tmux_pane("%1", "@0", "sh"),               // working pane
            tmux_pane("%2", "@0", SIDEBAR_PANE_TITLE), // its sidebar
            tmux_pane("%3", "@0", SIDEBAR_PANE_TITLE), // a duplicate sidebar
            tmux_pane("%4", "@1", SIDEBAR_PANE_TITLE), // a sidebar-only window
            host,                                      // managed daemon host
        ];
        let views = tmux_views_with_sidebars(&panes);
        assert_eq!(views.len(), 3, "windows stay in first-seen order");

        assert_eq!(views[0].view, "@0");
        assert!(views[0].has_working);
        assert_eq!(
            views[0].sidebar_panes,
            vec![
                PaneId::from_parts(MuxName::Tmux, "%2"),
                PaneId::from_parts(MuxName::Tmux, "%3"),
            ],
            "both sidebar panes, in order",
        );

        // window @1 is a sidebar-only orphan: no working pane and no daemon host.
        assert_eq!(views[1].view, "@1");
        assert!(
            !views[1].has_working,
            "a sidebar-only window holds no working pane",
        );
        assert!(!views[1].has_daemon_host);
        assert_eq!(views[1].sidebar_panes.len(), 1);

        assert_eq!(views[2].view, "@2");
        assert!(!views[2].has_working);
        assert!(
            views[2].has_daemon_host,
            "a daemon host marks the view so reload never collapses it as an orphan",
        );
        assert!(views[2].sidebar_panes.is_empty());
    }

    #[test]
    fn tmux_options_render_room_defaults() {
        let config = TmuxConfig::default();
        assert_eq!(
            tmux_server_options(&config),
            vec![
                ("focus-events", "on".to_owned()),
                ("set-clipboard", "on".to_owned()),
                ("extended-keys", "on".to_owned()),
                ("extended-keys-format", "csi-u".to_owned()),
                ("escape-time", "0".to_owned()),
            ],
        );
        assert_eq!(
            tmux_session_options(&config),
            vec![
                ("mouse", "on".to_owned()),
                ("history-limit", "100000".to_owned()),
                ("renumber-windows", "on".to_owned()),
            ],
        );
        assert_eq!(
            tmux_window_options(&config),
            vec![
                ("allow-passthrough", "on".to_owned()),
                ("aggressive-resize", "on".to_owned()),
                ("pane-border-status", "off".to_owned()),
                ("pane-border-lines", "simple".to_owned()),
            ],
        );
    }
}
