//! tmux room options and sidebar-pane classification.

use std::collections::HashMap;

use crate::config::TmuxConfig;
use crate::mux::{SidebarPaneOptions, ViewSidebars};
use crate::pane::PaneRef;

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

/// The session-scoped hook that docks the sidebar into every tmux window opened
/// after birth. Shared by launch and reconcile so the template cannot drift.
pub(super) fn after_new_window_hook_set_cmd(opts: &SidebarPaneOptions) -> Vec<String> {
    let serve = sidebar_serve_command(opts).join(" ");
    let split = format!(
        "split-window -h -b -d -l {} '{serve}'",
        opts.birth_size.cols
    );
    let mut hook_commands: Vec<String> = tmux_window_options(&opts.config.tmux)
        .into_iter()
        .map(|(key, value)| format!("set-window-option {key} '{}'", value))
        .collect();
    hook_commands.push(split);
    vec![
        "set-hook".to_owned(),
        "-t".to_owned(),
        opts.session_name.clone(),
        "after-new-window".to_owned(),
        hook_commands.join(" ; "),
    ]
}

pub(super) fn is_tmux_sidebar(pane: &PaneRef) -> bool {
    pane.command.as_deref() == Some(SIDEBAR_PANE_TITLE)
}

/// Group a pane list into per-window [`ViewSidebars`] for the reconcile planner:
/// each window's sidebar panes and whether it holds a user-working pane. Daemon
/// dashboard panes in `rimzd` are not work. Panes with no window id are skipped.
/// First-seen window order.
pub(super) fn tmux_views_with_sidebars(panes: &[PaneRef]) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
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

/// Server options Rimz appends so the user's existing array entries survive.
/// `*:extkeys` asks the outer terminal to send modified keys as CSI-u.
pub(super) fn tmux_server_append_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    let mut opts = Vec::new();
    if config.extended_keys {
        opts.push(("terminal-features", "*:extkeys".to_owned()));
    }
    opts
}

pub(super) fn tmux_session_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("mouse", tmux_bool(config.mouse)),
        ("history-limit", config.history_limit.to_string()),
        ("renumber-windows", tmux_bool(config.renumber_windows)),
    ]
}

/// `pane-border-format` Rimz writes when it owns `pane-border-status`: titled
/// frames on work panes, and a blank border line on the `rimz-sidebar` pane so
/// it reads frameless. `#{p999: }` floods the sidebar's border row with spaces
/// (truncated to pane width), overwriting the glyphs tmux would otherwise draw.
fn sidebar_blanking_border_format() -> String {
    [
        "#{?#{==:#{pane_title},",
        SIDEBAR_PANE_TITLE,
        "},#{p999: }, #{pane_index} #{pane_current_command} }",
    ]
    .concat()
}

pub(super) fn tmux_window_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    let mut opts = vec![
        ("allow-passthrough", tmux_bool(config.allow_passthrough)),
        ("aggressive-resize", tmux_bool(config.aggressive_resize)),
    ];
    if let Some(status) = config.pane_border_status {
        opts.push(("pane-border-status", status.as_str().to_owned()));
        if status != crate::config::TmuxPaneBorderStatus::Off {
            opts.push(("pane-border-format", sidebar_blanking_border_format()));
        }
    }
    if let Some(lines) = config.pane_border_lines {
        opts.push(("pane-border-lines", lines.as_str().to_owned()));
    }
    opts
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::config::MultiplexerConfig;
    use crate::ids::{MuxName, PaneId, WorkspaceId};
    use crate::mux::{SidebarPaneOptions, SidebarWidth};
    use crate::pane::PaneRef;

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
            resume_tabs: Vec::new(),
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
            is_floating: false,
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
    fn after_new_window_hook_threads_refresh_override_and_birth_width() {
        let opts = sidebar_opts(Some(75));
        let command = after_new_window_hook_set_cmd(&opts);
        let serve = sidebar_serve_command(&opts).join(" ");

        assert_eq!(
            command,
            vec![
                "set-hook".to_owned(),
                "-t".to_owned(),
                "room".to_owned(),
                "after-new-window".to_owned(),
                format!(
                    "set-window-option allow-passthrough 'on' ; \
                     set-window-option aggressive-resize 'on' ; \
                     split-window -h -b -d -l {} '{serve}'",
                    opts.birth_size.cols
                ),
            ],
        );
    }

    #[test]
    fn after_new_window_hook_replays_configured_window_options() {
        let mut opts = sidebar_opts(None);
        opts.config.tmux.pane_border_status = Some(crate::config::TmuxPaneBorderStatus::Top);
        opts.config.tmux.pane_border_lines = Some(crate::config::TmuxPaneBorderLines::Heavy);
        let command = after_new_window_hook_set_cmd(&opts);
        let serve = sidebar_serve_command(&opts).join(" ");

        assert_eq!(
            command,
            vec![
                "set-hook".to_owned(),
                "-t".to_owned(),
                "room".to_owned(),
                "after-new-window".to_owned(),
                format!(
                    "set-window-option allow-passthrough 'on' ; \
                     set-window-option aggressive-resize 'on' ; \
                     set-window-option pane-border-status 'top' ; \
                     set-window-option pane-border-format '{}' ; \
                     set-window-option pane-border-lines 'heavy' ; \
                     split-window -h -b -d -l {} '{serve}'",
                    sidebar_blanking_border_format(),
                    opts.birth_size.cols
                ),
            ],
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

        // window @1 is a sidebar-only orphan: no working pane and no daemon
        // infrastructure.
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
            "daemon infrastructure marks the view so reload never collapses it as an orphan",
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
            tmux_server_append_options(&config),
            vec![("terminal-features", "*:extkeys".to_owned())],
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
            ],
        );
        let config = TmuxConfig {
            pane_border_status: Some(crate::config::TmuxPaneBorderStatus::Top),
            pane_border_lines: Some(crate::config::TmuxPaneBorderLines::Heavy),
            ..TmuxConfig::default()
        };
        assert_eq!(
            tmux_window_options(&config),
            vec![
                ("allow-passthrough", "on".to_owned()),
                ("aggressive-resize", "on".to_owned()),
                ("pane-border-status", "top".to_owned()),
                ("pane-border-format", sidebar_blanking_border_format()),
                ("pane-border-lines", "heavy".to_owned()),
            ],
        );

        let config = TmuxConfig {
            pane_border_status: Some(crate::config::TmuxPaneBorderStatus::Off),
            ..TmuxConfig::default()
        };
        assert_eq!(
            tmux_window_options(&config),
            vec![
                ("allow-passthrough", "on".to_owned()),
                ("aggressive-resize", "on".to_owned()),
                ("pane-border-status", "off".to_owned()),
            ],
        );

        let config = TmuxConfig {
            extended_keys: false,
            ..TmuxConfig::default()
        };
        assert!(tmux_server_append_options(&config).is_empty());
    }
}
