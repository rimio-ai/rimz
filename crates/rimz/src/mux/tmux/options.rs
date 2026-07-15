//! tmux room options and sidebar-pane classification.

use std::collections::HashMap;
use std::num::NonZeroU16;
use std::path::Path;

use crate::config::{TmuxConfig, TmuxExtendedKeysFormat};
use crate::ids::MuxName;
use crate::mux::{SidebarPaneOptions, ViewSidebars, sidebar_serve_args};
use crate::pane::{PaneRef, SIDEBAR_CHROME_TITLE};

pub(super) const SIDEBAR_WIDTH_OPTION: &str = "@rimz_sidebar_cols";

/// The `rimz sidebar serve …` argv a tmux sidebar pane runs. Shared by initial
/// launch and in-place recovery so the two cannot drift.
pub(super) fn sidebar_serve_command(opts: &SidebarPaneOptions) -> Vec<String> {
    let mut command = vec![opts.rimz_bin.to_string_lossy().into_owned()];
    command.extend(sidebar_serve_args(MuxName::Tmux, opts));
    command
}

/// Fresh tmux birth path: repurpose the session's pristine first pane as the
/// sidebar, then create the work shell to its right at its final width.
pub(super) fn birth_split_commands(
    sidebar_pane: &str,
    sidebar_cols: NonZeroU16,
    window_width: u64,
    cwd: &Path,
    sidebar_argv: &[String],
) -> Vec<Vec<String>> {
    let shell_width = window_width
        .saturating_sub(u64::from(sidebar_cols.get()) + 1)
        .max(1);
    let mut respawn = vec![
        "respawn-pane".to_owned(),
        "-k".to_owned(),
        "-t".to_owned(),
        sidebar_pane.to_owned(),
    ];
    respawn.extend(sidebar_argv.iter().cloned());
    let split = vec![
        "split-window".to_owned(),
        "-h".to_owned(),
        "-l".to_owned(),
        shell_width.to_string(),
        "-t".to_owned(),
        sidebar_pane.to_owned(),
        "-c".to_owned(),
        cwd.to_string_lossy().into_owned(),
    ];
    vec![respawn, split]
}

/// The session-scoped hook that docks the sidebar into every tmux window opened
/// after birth. Shared by launch and reconcile so the template cannot drift.
pub(super) fn after_new_window_hook_set_cmd(opts: &SidebarPaneOptions) -> Vec<String> {
    let serve = sidebar_serve_command(opts).join(" ");
    let split = format!("split-window -h -b -d -l '#{{{SIDEBAR_WIDTH_OPTION}}}' '{serve}'");
    let mut hook_commands: Vec<String> = tmux_window_options(&opts.config.tmux)
        .into_iter()
        .map(|(key, value)| format!("set-window-option {key} '{}'", value))
        .collect();
    hook_commands.push(split);
    // A plain default-shell tab has an empty `pane_start_command`: tmux births
    // it full width, then the sidebar split above shrinks it after zsh can draw
    // a prompt and surface PROMPT_SP's `%` marker. Respawn only that plain work
    // pane as a fresh shell at the post-split width. Explicit-command windows
    // keep their process and layout.
    let shell = crate::harness::launch::user_shell_program();
    hook_commands.push(format!(
        "if-shell -F '#{{pane_start_command}}' '' 'respawn-pane -k \"{shell}\"'"
    ));
    vec![
        "set-hook".to_owned(),
        "-t".to_owned(),
        opts.session_name.clone(),
        "after-new-window".to_owned(),
        hook_commands.join(" ; "),
    ]
}

/// Set the live absolute-column target consumed by the `after-new-window`
/// hook. Keeping the mutable width in a session option lets renderer target
/// recording refresh future births without reconstructing its command.
pub(super) fn sidebar_width_option_set_cmd(
    session: &str,
    cols: std::num::NonZeroU16,
) -> Vec<String> {
    vec![
        "set-option".to_owned(),
        "-t".to_owned(),
        session.to_owned(),
        SIDEBAR_WIDTH_OPTION.to_owned(),
        cols.to_string(),
    ]
}

/// One-shot hook for pristine tmux birth. The first real client attach respawns
/// the birth work shell so its first prompt lands after the attach resize
/// settles, then removes the hook. Control-mode clients only carry presence
/// wakeups, so they leave the one-shot armed for the user's first pty attach.
pub(super) fn birth_shell_cleanup_hook_set_cmd(session: &str, work_pane: &str) -> Vec<String> {
    let body = format!(
        "if-shell -F '#{{client_control_mode}}' '' \
         'respawn-pane -k -t {work_pane} ; set-hook -u client-attached'"
    );
    vec![
        "set-hook".to_owned(),
        "-t".to_owned(),
        session.to_owned(),
        "client-attached".to_owned(),
        body,
    ]
}

pub(super) fn is_tmux_sidebar(pane: &PaneRef) -> bool {
    pane.command.as_deref() == Some(SIDEBAR_CHROME_TITLE)
}

/// Group a pane list into per-window [`ViewSidebars`] for the reconcile planner:
/// each window's sidebar panes and whether it holds a user-working pane. Daemon
/// dashboard panes in `rimzd` are not work. Panes with no window id are skipped.
/// First-seen window order.
///
/// Session-scoped pane reads should already contain only this room; the guard
/// keeps reconcile safe against fixture leaks and backend regressions.
pub(super) fn tmux_views_with_sidebars(panes: &[PaneRef], session: &str) -> Vec<ViewSidebars> {
    let mut views: Vec<ViewSidebars> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for pane in panes {
        if pane.session_name != session {
            continue;
        }
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
        } else if crate::daemon_view::pane_is_host(pane) {
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
    let mut opts = vec![
        ("focus-events", tmux_bool(config.focus_events)),
        ("set-clipboard", config.set_clipboard.as_str().to_owned()),
        ("extended-keys", tmux_bool(config.extended_keys)),
        (
            "extended-keys-format",
            config.extended_keys_format.as_str().to_owned(),
        ),
        ("escape-time", config.escape_time_ms.to_string()),
    ];
    if config.extended_keys {
        // tmux accepts `ESC[27;1u` as Escape but leaks Ghostty's bare
        // `ESC[27u`; name that sequence so the root binding can normalize it.
        opts.push(("user-keys[240]", "\u{1b}[27u".to_owned()));
    }
    opts
}

/// Server options RimZ appends so the user's existing array entries survive.
/// `*:sync` enables atomic redraws, and `*:extkeys` asks the outer terminal to
/// send modified keys.
pub(super) fn tmux_server_append_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    let mut opts = Vec::new();
    opts.push(("terminal-features", "*:sync".to_owned()));
    if config.extended_keys {
        opts.push(("terminal-features", "*:extkeys".to_owned()));
    }
    opts
}

pub(super) fn tmux_extended_key_bindings(config: &TmuxConfig) -> Vec<Vec<String>> {
    if !config.extended_keys {
        return Vec::new();
    }

    let (shift_enter, alt_enter) = match config.extended_keys_format {
        TmuxExtendedKeysFormat::CsiU => ("[13;2u", "[13;3u"),
        TmuxExtendedKeysFormat::Xterm => ("[27;2;13~", "[27;3;13~"),
    };

    let mut bindings: Vec<Vec<String>> = [("S-Enter", shift_enter), ("M-Enter", alt_enter)]
        .into_iter()
        .map(|(key, sequence)| {
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                key.to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                sequence.to_owned(),
            ]
        })
        .collect();
    bindings.push(vec![
        "bind-key".to_owned(),
        "-n".to_owned(),
        "User240".to_owned(),
        "send-keys".to_owned(),
        "Escape".to_owned(),
    ]);
    bindings
}

pub(super) fn tmux_session_options(config: &TmuxConfig) -> Vec<(&'static str, String)> {
    vec![
        ("mouse", tmux_bool(config.mouse)),
        ("history-limit", config.history_limit.to_string()),
        ("renumber-windows", tmux_bool(config.renumber_windows)),
    ]
}

/// `pane-border-format` RimZ writes when it owns `pane-border-status`: titled
/// frames on work panes, and a blank border line on the sidebar chrome pane so
/// it reads frameless. `#{p999: }` floods the sidebar's border row with spaces
/// (truncated to pane width), overwriting the glyphs tmux would otherwise draw.
fn sidebar_blanking_border_format() -> String {
    [
        "#{?#{==:#{pane_title},",
        SIDEBAR_CHROME_TITLE,
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
            extra_env: Default::default(),
            cwd: PathBuf::from("/tmp/rimz-tmux-refresh"),
            width,
            birth_size: width.birth_size(None),
            width_override: None,
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            pristine_birth: false,
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
            title: None,
            is_focused: false,
            is_floating: false,
            command: Some(command.to_owned()),
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
    fn birth_split_respawns_sidebar_then_splits_work_shell_at_final_width() {
        let sidebar_argv = sidebar_serve_command(&sidebar_opts(None));
        let commands = birth_split_commands(
            "%0",
            NonZeroU16::new(24).expect("nonzero"),
            100,
            Path::new("/tmp/rimz-tmux-refresh"),
            &sidebar_argv,
        );

        let mut expected_respawn = vec![
            "respawn-pane".to_owned(),
            "-k".to_owned(),
            "-t".to_owned(),
            "%0".to_owned(),
        ];
        expected_respawn.extend(sidebar_argv);
        assert_eq!(
            commands,
            vec![
                expected_respawn,
                vec![
                    "split-window".to_owned(),
                    "-h".to_owned(),
                    "-l".to_owned(),
                    "75".to_owned(),
                    "-t".to_owned(),
                    "%0".to_owned(),
                    "-c".to_owned(),
                    "/tmp/rimz-tmux-refresh".to_owned(),
                ],
            ],
        );
    }

    #[test]
    fn birth_split_clamps_work_shell_width_to_one_column() {
        let commands = birth_split_commands(
            "%0",
            NonZeroU16::new(24).expect("nonzero"),
            20,
            Path::new("/tmp/rimz-tmux-refresh"),
            &["rimz".to_owned()],
        );

        assert_eq!(commands[1][3], "1");
    }

    #[test]
    fn after_new_window_hook_threads_refresh_override_width_option_and_options() {
        let opts = sidebar_opts(Some(75));
        let command = after_new_window_hook_set_cmd(&opts);
        let serve = sidebar_serve_command(&opts).join(" ");
        let shell = crate::harness::launch::user_shell_program();

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
                     split-window -h -b -d -l '#{{@rimz_sidebar_cols}}' '{serve}' ; \
                     if-shell -F '#{{pane_start_command}}' '' 'respawn-pane -k \"{shell}\"'"
                ),
            ],
        );

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
                     split-window -h -b -d -l '#{{@rimz_sidebar_cols}}' '{serve}' ; \
                     if-shell -F '#{{pane_start_command}}' '' 'respawn-pane -k \"{shell}\"'",
                    sidebar_blanking_border_format()
                ),
            ],
        );
    }

    #[test]
    fn sidebar_width_option_sets_absolute_cols() {
        assert_eq!(
            sidebar_width_option_set_cmd("room", NonZeroU16::new(36).expect("nonzero")),
            vec!["set-option", "-t", "room", "@rimz_sidebar_cols", "36"],
        );
    }

    #[test]
    fn birth_shell_cleanup_hook_builds_one_shot_guarded_respawn() {
        assert_eq!(
            birth_shell_cleanup_hook_set_cmd("room", "%7"),
            vec![
                "set-hook".to_owned(),
                "-t".to_owned(),
                "room".to_owned(),
                "client-attached".to_owned(),
                "if-shell -F '#{client_control_mode}' '' \
                 'respawn-pane -k -t %7 ; set-hook -u client-attached'"
                    .to_owned(),
            ],
        );
    }

    #[test]
    fn views_with_sidebars_classifies_working_orphan_and_daemon_windows() {
        let mut host = tmux_pane("%5", "@2", "rimz");
        host.view_name = Some(crate::daemon_view::VIEW_NAME.to_owned());
        let mut foreign = tmux_pane("%6", "@9", SIDEBAR_CHROME_TITLE);
        foreign.session_name = "other-room".to_owned();
        let panes = vec![
            tmux_pane("%1", "@0", "sh"),                 // working pane
            tmux_pane("%2", "@0", SIDEBAR_CHROME_TITLE), // its sidebar
            tmux_pane("%3", "@0", SIDEBAR_CHROME_TITLE), // a duplicate sidebar
            tmux_pane("%4", "@1", SIDEBAR_CHROME_TITLE), // a sidebar-only window
            host,                                        // managed daemon host
            foreign,                                     // another tmux session on the server
        ];
        let views = tmux_views_with_sidebars(&panes, "room");
        assert_eq!(views.len(), 3, "windows stay in first-seen order");
        assert!(
            views.iter().all(|view| view.view != "@9"),
            "foreign-session windows are excluded before planning",
        );
        assert!(
            views.iter().all(|view| {
                !view
                    .sidebar_panes
                    .contains(&PaneId::from_parts(MuxName::Tmux, "%6"))
            }),
            "foreign-session sidebars are excluded before planning",
        );

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
                ("user-keys[240]", "\u{1b}[27u".to_owned()),
            ],
        );
        assert_eq!(
            tmux_server_append_options(&config),
            vec![
                ("terminal-features", "*:sync".to_owned()),
                ("terminal-features", "*:extkeys".to_owned()),
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
        assert!(
            tmux_server_options(&config)
                .iter()
                .all(|(key, _)| *key != "user-keys[240]"),
        );
        assert_eq!(
            tmux_server_append_options(&config),
            vec![("terminal-features", "*:sync".to_owned())],
        );
    }
}
