use super::*;

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
fn tmux_soft_newline_bindings_follow_extended_key_format() {
    let csi_u = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::CsiU,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_soft_newline_bindings(&csi_u),
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
        ],
    );

    let xterm = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::Xterm,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_soft_newline_bindings(&xterm),
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
        ],
    );

    let disabled = crate::config::TmuxConfig {
        extended_keys: false,
        ..Default::default()
    };
    assert!(options::tmux_soft_newline_bindings(&disabled).is_empty());
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
        rimz_bin: PathBuf::from("/bin/true"),
        replace_existing: false,
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
