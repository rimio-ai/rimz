use super::*;

#[test]
fn default_server_socket_path_uses_tmux_default_layout() {
    assert_eq!(
        default_server_socket_path_from(Path::new("/tmp"), 1001),
        PathBuf::from("/tmp/tmux-1001/default"),
    );
}

#[test]
fn equal_row_splits_size_each_remaining_stack() {
    let sizes = |pane_count| {
        (1..pane_count)
            .map(|index| backend::equal_row_split_size(pane_count, index))
            .collect::<Vec<_>>()
    };

    assert_eq!(sizes(2), ["50%"]);
    assert_eq!(sizes(3), ["66%", "50%"]);
    assert_eq!(sizes(4), ["75%", "66%", "50%"]);
}

#[test]
fn version_parser_and_floor_hold() {
    assert_eq!(parse_version("tmux 3.5a"), Some((3, 5, 0)));
    assert_eq!(parse_version("tmux 3.2"), Some((3, 2, 0)));
    assert_eq!(parse_version("  tmux 3.4  \n"), Some((3, 4, 0)));
    assert_eq!(parse_version("tmux 2.9a"), Some((2, 9, 0)));
    assert_eq!(parse_version("garbage"), None);

    assert!((3, 5, 0) >= MIN_TMUX_VERSION);
    assert!((3, 6, 0) >= MIN_TMUX_VERSION);
    // 3.4 lacks `extended-keys-format`, which the room options still set
    // across all supported hosts — below the floor.
    assert!((3, 4, 0) < MIN_TMUX_VERSION);
    assert!((3, 2, 0) < MIN_TMUX_VERSION);
}

#[test]
fn log_classifier_matches_error_and_fatal_mentions() {
    use crate::mux::logtail::LogSeverity;

    assert_eq!(
        classify_log_line("server error: client lost"),
        Some(LogSeverity::Error)
    );
    assert_eq!(
        classify_log_line("fatal: control socket closed"),
        Some(LogSeverity::Error)
    );
    assert_eq!(classify_log_line("normal redraw"), None);
}

#[test]
fn tmux_extended_key_bindings_follow_extended_key_format() {
    let csi_u = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::CsiU,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_extended_key_bindings(&csi_u),
        vec![
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "S-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[13;2u".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "M-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[13;3u".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "User240".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
            ],
        ],
    );

    let xterm = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::Xterm,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_extended_key_bindings(&xterm),
        vec![
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "S-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[27;2;13~".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "M-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[27;3;13~".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "User240".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
            ],
        ],
    );

    let disabled = crate::config::TmuxConfig {
        extended_keys: false,
        ..Default::default()
    };
    assert!(options::tmux_extended_key_bindings(&disabled).is_empty());
}

#[test]
fn window_name_neutralizes_tmux_target_separators() {
    use super::window::sanitize_window_name;

    // tmux parses `:` as session:window and `.` as window.pane in a target
    // spec, so `new-window -n` rejects a name carrying either. The run-pane
    // title and channel labels are human text that can carry both.
    assert_eq!(sanitize_window_name("run: codex"), "run- codex");
    assert_eq!(sanitize_window_name("feat: split.ci"), "feat- split-ci");
    assert_eq!(sanitize_window_name("plain-name"), "plain-name");
}

#[test]
fn open_tab_rejects_an_empty_layout() {
    use std::path::{Path, PathBuf};

    use crate::ids::WorkspaceId;
    use crate::mux::{
        LayoutColumn, LayoutPanes, MuxBackend, MuxErr, PaneCmd, SidebarPaneOptions, SidebarWidth,
        TabOptions,
    };

    // Pointed at a socket no server owns: the empty-layout guards return before
    // any tmux command runs, so this never forks tmux and needs no live server.
    let backend = TmuxBackend::with_socket("/nonexistent/rimz-open-tab.sock");
    let width = SidebarWidth::default();
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-empty".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-empty")),
        project_root: PathBuf::from("/tmp/rimz-empty"),
        cwd: PathBuf::from("/tmp/rimz-empty"),
        width,
        birth_size: width.birth_size(Some(80)),
        width_override: None,
        rimz_bin: PathBuf::from("/bin/true"),
        replace_existing: false,
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let tab = |columns: Vec<Vec<PaneCmd>>| TabOptions {
        session_name: "rimz-empty".to_owned(),
        title: "work".to_owned(),
        cwd: PathBuf::from("/tmp/rimz-empty"),
        panes: LayoutPanes {
            columns: columns
                .into_iter()
                .map(|panes| LayoutColumn {
                    panes,
                    stacked: false,
                })
                .collect(),
        },
        focus: true,
        dock_sidebar: true,
        sidebar: sidebar.clone(),
    };

    let err = backend
        .open_tab(&tab(Vec::new()))
        .expect_err("no columns must error");
    assert!(
        matches!(err, MuxErr::Output { ref reason, .. } if reason.contains("no columns")),
        "expected a no-columns Output error, got {err:?}",
    );

    let err = backend
        .open_tab(&tab(vec![Vec::new()]))
        .expect_err("an empty column must error");
    assert!(
        matches!(err, MuxErr::Output { ref reason, .. } if reason.contains("empty column")),
        "expected an empty-column Output error, got {err:?}",
    );
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = TmuxBackend::default();
    backend
        .version
        .set("tmux 9.9".to_owned())
        .expect("a fresh instance has not probed yet");
    // The cache is consulted before any probe: the seeded value comes back
    // verbatim — no `tmux -V` fork, no overwrite by a real binary.
    assert_eq!(backend.version().expect("cached version"), "tmux 9.9");
}

#[test]
fn list_panes_scopes_session_without_server_wide_flag() {
    let backend = TmuxBackend::default();

    let session_args = backend.list_panes_command(Some("rimz-room")).args;
    assert_eq!(
        &session_args[..5],
        ["list-panes", "-s", "-t", "rimz-room", "-F"]
    );
    assert!(!session_args.iter().any(|arg| arg == "-a"));

    let server_args = backend.list_panes_command(None).args;
    assert_eq!(&server_args[..3], ["list-panes", "-a", "-F"]);
}

#[test]
fn sidebar_geometry_probe_is_one_session_scoped_command() {
    let backend = TmuxBackend::default();

    assert_eq!(
        backend.session_pane_geometries_command("rimz-room").args,
        [
            "list-panes",
            "-s",
            "-t",
            "rimz-room",
            "-F",
            "#{pane_id} #{window_id} #{pane_width} #{window_width} #{==:#{pane_title},rimz-sidebar}",
        ],
    );
}

#[test]
fn sidebar_geometry_probe_parser_requires_five_typed_fields() {
    use super::window::{TmuxPaneGeometry, parse_tmux_pane_geometry};

    assert_eq!(
        parse_tmux_pane_geometry("%3 @1 72 240 1"),
        Some(TmuxPaneGeometry {
            pane_id: "%3".to_owned(),
            window_id: "@1".to_owned(),
            pane_width: 72,
            window_width: 240,
            is_sidebar: true,
        }),
    );
    assert_eq!(parse_tmux_pane_geometry("%3 @1 wide 240 1"), None);
    assert_eq!(parse_tmux_pane_geometry("%3 @1 72 240 0 extra"), None);
}
