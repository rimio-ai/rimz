use super::*;

/// Argv past the `-S <socket>` prefix every managed command carries, so a verb
/// assertion stays about the verb. [`managed_endpoint_prefixes_every_command`]
/// owns the prefix itself.
fn verb_args(spec: &CommandSpec) -> &[String] {
    assert_eq!(
        &spec.args[..1],
        ["-S"],
        "every tmux command must address an explicit socket",
    );
    &spec.args[2..]
}

#[test]
fn managed_endpoint_prefixes_every_command() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.cmd();

    assert_eq!(
        &spec.args[..2],
        ["-S", "/run/user/1000/rimz/tmux/server"],
        "commands address the RimZ-owned server, never the user's default",
    );
    // A tmux server inherits its cwd from the client that births it, and only
    // honours a pane's `-c` while `getcwd()` succeeds. Birth from a directory
    // that can be deleted strands every later pane there.
    assert_eq!(spec.cwd.as_deref(), Some(Path::new("/")));
    assert!(
        spec.env_remove.contains("TMUX"),
        "an inherited $TMUX would let an ambient server capture a managed command",
    );
}

#[test]
fn existing_session_attach_targets_the_managed_server() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.attach_existing_command("rimz-test");

    assert_eq!(verb_args(&spec), ["attach", "-t", "rimz-test"]);
}

#[test]
fn rename_tab_targets_the_anchor_pane_and_encodes_the_name() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let spec = backend
        .rename_window_command(&pane, "#feat:one.2 ✓")
        .expect("tmux pane");

    assert_eq!(
        verb_args(&spec),
        ["rename-window", "-t", "%7", "##feat-one-2 ✓"]
    );
}

#[test]
fn tab_status_commands_probe_and_remember_automatic_rename() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");

    let automatic = backend
        .automatic_rename_probe_command(&pane)
        .expect("tmux pane");
    assert_eq!(
        verb_args(&automatic),
        ["display-message", "-p", "-t", "%7", "#{automatic-rename}"]
    );

    let rename = backend
        .rename_window_with_restore_marker_command(&pane, "#feat:one.2 ✓")
        .expect("tmux pane");
    assert_eq!(
        verb_args(&rename),
        [
            "set-option",
            "-w",
            "-t",
            "%7",
            "@rimz_restore_automatic_rename",
            "on",
            ";",
            "rename-window",
            "-t",
            "%7",
            "##feat-one-2 ✓",
        ]
    );
}

#[test]
fn tab_status_clear_commands_probe_and_restore_automatic_rename() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");

    let marker = backend
        .restore_automatic_rename_probe_command(&pane)
        .expect("tmux pane");
    assert_eq!(
        verb_args(&marker),
        [
            "show-options",
            "-wqv",
            "-t",
            "%7",
            "@rimz_restore_automatic_rename",
        ]
    );

    let clear = backend
        .clear_window_status_and_restore_command(&pane, "#feat:one.2")
        .expect("tmux pane");
    assert_eq!(
        verb_args(&clear),
        [
            "rename-window",
            "-t",
            "%7",
            "##feat-one-2",
            ";",
            "set-option",
            "-w",
            "-t",
            "%7",
            "automatic-rename",
            "on",
            ";",
            "set-option",
            "-wu",
            "-t",
            "%7",
            "@rimz_restore_automatic_rename",
        ]
    );
}

#[test]
fn readonly_attach_blocks_input_and_ignores_viewer_size() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    backend
        .version
        .set("tmux 3.5".to_owned())
        .expect("fresh version cache");
    let spec = backend.attach_readonly_command("rimz-test");

    assert_eq!(
        verb_args(&spec),
        ["attach", "-t", "rimz-test", "-r", "-f", "ignore-size"]
    );
}

#[test]
fn the_managed_endpoint_needs_no_workspace_to_reconstruct() {
    // Any caller rebuilds the same endpoint from the runtime domain alone —
    // this is what keeps `backend_for(MuxName)` free of a workspace argument.
    let runtime_root = Path::new("/run/user/1000");
    assert_eq!(
        managed_server_socket_path_under(runtime_root),
        PathBuf::from("/run/user/1000/rimz/tmux/server"),
    );
    // A disposable runtime root yields a private server, which is what gives
    // sandboxes and tests their isolation for free.
    assert_ne!(
        managed_server_socket_path_under(Path::new("/tmp/rimz-sandbox/runtime")),
        managed_server_socket_path_under(runtime_root),
    );
}

#[test]
fn legacy_conflict_recovery_is_scoped_to_the_one_session() {
    let conflict = LegacySessionConflict {
        session: "rimz-project-a1b2c3".to_owned(),
        socket: PathBuf::from("/tmp/tmux-1000/default"),
    };

    // Session-scoped on purpose: `kill-server` here would destroy the user's
    // own unrelated sessions, which RimZ does not own.
    assert_eq!(
        conflict.recovery_command(),
        "tmux -S /tmp/tmux-1000/default kill-session -t rimz-project-a1b2c3",
    );
    assert!(!conflict.recovery_command().contains("kill-server"));
}

#[test]
fn default_server_socket_path_uses_tmux_default_layout() {
    assert_eq!(
        default_server_socket_path_from(Path::new("/tmp"), 1001),
        PathBuf::from("/tmp/tmux-1001/default"),
    );
}

#[test]
fn tmux_var_parser_extracts_a_nonempty_socket() {
    assert_eq!(
        socket_path_from_tmux_var("/tmp/tmux-1001/default,42,3"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(
        socket_path_from_tmux_var("/tmp/tmux-1001/default"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(
        socket_path_from_tmux_var(" /tmp/tmux-1001/default ,42,3"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(socket_path_from_tmux_var(",42,3"), None);
    assert_eq!(socket_path_from_tmux_var(""), None);
}

#[test]
fn equal_row_splits_size_each_remaining_stack() {
    let sizes = |pane_count| {
        (1..pane_count)
            .map(|index| window::equal_row_split_size(pane_count, index))
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
fn window_name_arg_encodes_literal_hashes_after_sanitizing() {
    use super::window::window_name_arg;

    assert_eq!(window_name_arg("#feat: split.ci##"), "##feat- split-ci####");
    assert_eq!(window_name_arg("plain-name"), "plain-name");
}

#[test]
fn open_tab_rejects_an_empty_layout() {
    use std::path::{Path, PathBuf};

    use crate::ids::WorkspaceId;
    use crate::mux::{
        LayoutColumn, LayoutPanes, MuxBackend, MuxErr, PaneCmd, SidebarPaneOptions, TabOptions,
    };

    // Pointed at a socket no server owns: the empty-layout guards return before
    // any tmux command runs, so this never forks tmux and needs no live server.
    let backend = TmuxBackend::with_socket("/nonexistent/rimz-open-tab.sock");
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-empty".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-empty")),
        project_root: PathBuf::from("/tmp/rimz-empty"),
        extra_env: Default::default(),
        cwd: PathBuf::from("/tmp/rimz-empty"),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(20).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: PathBuf::from("/bin/true"),
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let tab = |columns: Vec<Vec<PaneCmd>>| TabOptions {
        title: "work".to_owned(),
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

    let session_spec = backend.list_panes_command(Some("rimz-room"));
    let session_args = verb_args(&session_spec);
    assert_eq!(
        &session_args[..5],
        ["list-panes", "-s", "-t", "rimz-room", "-F"]
    );
    assert!(!session_args.iter().any(|arg| arg == "-a"));

    let server_spec = backend.list_panes_command(None);
    assert_eq!(&verb_args(&server_spec)[..3], ["list-panes", "-a", "-F"]);
}

#[test]
fn client_view_uses_a_printable_field_separator() {
    let spec = TmuxBackend::default().client_view_command(Some("rimz-room"));
    assert_eq!(
        verb_args(&spec),
        [
            "list-clients",
            "-F",
            "#{client_name}|#{pane_id}|#{client_activity}|#{client_flags}",
            "-t",
            "rimz-room",
        ],
    );
}

#[test]
fn sidebar_geometry_probe_is_one_session_scoped_command() {
    let backend = TmuxBackend::default();

    let spec = backend.session_pane_geometries_command("rimz-room");
    assert_eq!(
        verb_args(&spec),
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
