use super::*;

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
#[test]
fn pane_content_size_reads_dimensions_and_preserves_zero() {
    for (rows, cols) in [(38, 118), (0, 118)] {
        let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$(dirname \"$0\")/argv\"\nprintf '{rows} {cols}\\n'\n"
        ));
        let mut backend = TmuxBackend::with_socket("/test/socket");
        backend.program = Some(shim);
        let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
        assert_eq!(
            backend
                .pane_content_size(&pane, None, Duration::from_secs(2))
                .ok(),
            Some(Some(crate::mux::PaneContentSize { rows, cols }))
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("argv"))
                .unwrap()
                .trim(),
            "-S /test/socket -u display-message -p -t %7 #{pane_height} #{pane_width}"
        );
    }
}

#[cfg(unix)]
#[test]
fn pane_content_size_distinguishes_missing_pane_from_command_failure() {
    for (stderr, missing) in [("can't find pane: %7", true), ("server unavailable", false)] {
        let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            "#!/bin/sh\nprintf '%s\\n' \"{stderr}\" >&2\nexit 1\n"
        ));
        let mut backend = TmuxBackend::with_socket("/test/socket");
        backend.program = Some(shim);
        let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
        let result = backend.pane_content_size(&pane, None, Duration::from_secs(2));
        if missing {
            assert_eq!(result.ok(), Some(None));
        } else {
            assert!(result.is_err());
        }
    }
}

#[cfg(unix)]
#[test]
fn command_failure_omits_socket_from_native_stderr() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shim = temp.path().join("tmux");
    std::fs::write(&shim, "#!/bin/sh\nprintf 'error connecting to %s (No such file or directory)\\n' \"$2\" >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = temp.path().join("socket with spaces");
    let mut spec = TmuxBackend::with_socket(&socket)
        .cmd()
        .args(["capture-pane", "-p"]);
    spec.program = shim.to_string_lossy().into_owned();
    let error = spec.run().unwrap_err().to_string();
    assert!(!error.contains("socket with spaces"), "{error}");
    assert!(!error.contains("spaces capture-pane"), "{error}");
    assert!(error.contains("No such file or directory"), "{error}");
}

fn session_options() -> crate::mux::SessionOptions {
    crate::mux::SessionOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/project")),
        project_root: PathBuf::from("/project"),
        extra_env: Default::default(),
        cwd: PathBuf::from("/project"),
        config: Default::default(),
        detected_size: None,
        truecolor: false,
    }
}

/// Argv past the `-S <socket> -u` prefix every managed command carries, so a verb
/// assertion stays about the verb. [`managed_endpoint_prefixes_every_command`]
/// owns the prefix itself.
fn verb_args(spec: &CommandSpec) -> &[String] {
    assert_eq!(
        &spec.args[..1],
        ["-S"],
        "every tmux command must address an explicit socket",
    );
    assert_eq!(
        spec.args.get(2).map(String::as_str),
        Some("-u"),
        "every tmux command must preserve UTF-8 regardless of the caller's locale",
    );
    &spec.args[3..]
}

#[test]
fn managed_endpoint_prefixes_every_command() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.cmd();

    assert_eq!(
        spec.args,
        ["-S", "/run/user/1000/rimz/tmux/server", "-u"],
        "commands address the RimZ-owned server with a UTF-8 client",
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
fn new_session_birth_removes_outer_mux_context() {
    let opts = session_options();
    let spec =
        TmuxBackend::with_socket("/test/socket").new_session_command(&opts, &Default::default());
    for key in crate::mux::AMBIENT_MUX_ENV {
        assert!(
            spec.env_remove.contains(key),
            "{key} remains inherited at birth"
        );
    }
    assert_eq!(verb_args(&spec)[0], "new-session");
}

#[test]
fn existing_session_attach_targets_the_managed_server() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.attach_existing_command("rimz-test");

    assert_eq!(verb_args(&spec), ["attach", "-t", "rimz-test"]);
}

#[test]
fn global_mux_context_removal_marks_all_five_names() {
    let spec = TmuxBackend::with_socket("/test/socket").global_mux_context_removal_command();
    let commands = verb_args(&spec).split(|arg| arg == ";").collect::<Vec<_>>();
    let expected = [
        "TMUX",
        "TMUX_PANE",
        "ZELLIJ",
        "ZELLIJ_PANE_ID",
        "ZELLIJ_SESSION_NAME",
    ];
    assert_eq!(commands.len(), expected.len());
    for (command, key) in commands.into_iter().zip(expected) {
        assert_eq!(command, ["set-environment", "-g", "-r", key]);
    }
}

#[cfg(unix)]
#[test]
fn ensure_session_repairs_global_mux_context_before_and_after_birth() {
    for (pre_birth_error, duplicate) in [
        ("", false),
        ("", true),
        ("no server running on server", false),
        (
            "error connecting to server (No such file or directory)",
            false,
        ),
    ] {
        let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/argv"
if [ "$4" = set-environment ] && [ "$5" = -g ] && [ ! -e "$dir/repaired" ]; then
    touch "$dir/repaired"
    if [ -n '{pre_birth_error}' ]; then printf '%s\n' '{pre_birth_error}' >&2; exit 1; fi
fi
if [ "$4" = new-session ] && [ '{duplicate}' = true ]; then
    printf 'duplicate session: rimz-test\n' >&2
    exit 1
fi
"#
        ));
        let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
        backend.program = Some(shim);
        let opts = session_options();
        backend.ensure_session(&opts).expect("ensure session");
        let log = std::fs::read_to_string(temp.path().join("argv")).unwrap();
        let commands = log.lines().collect::<Vec<_>>();
        assert!(
            commands[0].contains("set-environment -g -r TMUX"),
            "repair must precede new-session: {log}"
        );
        let birth = commands
            .iter()
            .position(|line| line.contains(" new-session "))
            .unwrap();
        let repair = backend.global_mux_context_removal_command().args.join(" ");
        assert_eq!(commands[0], repair);
        assert!(
            commands[birth + 1..].contains(&repair.as_str()),
            "post-birth repair: {log}"
        );
    }
}

#[cfg(unix)]
#[test]
fn ensure_session_does_not_ignore_global_environment_repair_failure() {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\nif [ \"$4\" = set-environment ] && [ \"$5\" = -g ]; then printf 'error connecting to server (Permission denied)\\n' >&2; exit 1; fi\n",
    );
    let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
    backend.program = Some(shim);
    let opts = session_options();
    assert!(
        backend.ensure_session(&opts).is_err(),
        "only the pre-birth no-server error may be ignored"
    );
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
fn tab_claim_pins_the_sanitized_pane_name_not_the_tab_name() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let claim = backend
        .claim_window_command(&pane, "#feat:one.2", "opus.fast")
        .expect("tmux pane");
    assert_eq!(
        verb_args(&claim),
        [
            "set-option",
            "-p",
            "-t",
            "%7",
            "@rimz_title",
            "opus-fast",
            ";",
            "rename-window",
            "-t",
            "%7",
            "##feat-one-2",
            ";",
            "set-option",
            "-w",
            "-t",
            "%7",
            "@rimz_tab_base",
            "#feat-one-2",
            ";",
            "set-option",
            "-w",
            "-t",
            "%7",
            "@rimz_tab_founders",
            "%7",
        ]
    );
}

#[test]
fn tab_release_lists_the_anchor_window_and_clears_each_pane_pin() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let list = backend.window_pane_ids_command(&pane).expect("tmux pane");
    assert_eq!(
        verb_args(&list),
        ["list-panes", "-t", "%7", "-F", "#{pane_id}"]
    );
    let release = backend
        .release_window_command(&pane, "zsh", &["%7".to_owned(), "%8".to_owned()])
        .expect("tmux pane");
    assert_eq!(
        verb_args(&release)[15..],
        [
            "@rimz_tab_base",
            ";",
            "set-option",
            "-wu",
            "-t",
            "%7",
            "@rimz_tab_founders",
            ";",
            "set-option",
            "-pu",
            "-t",
            "%7",
            "@rimz_title",
            ";",
            "set-option",
            "-pu",
            "-t",
            "%8",
            "@rimz_title",
        ]
    );
}

#[test]
fn rebuild_updates_the_base_inside_the_observed_name_guard() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let rebuild = backend
        .rebuild_window_command(&pane, "peer.fast ?", "peer-fast")
        .expect("tmux pane");
    let guarded = backend
        .window_name_guarded_command(&pane, "founder", "peer.fast ?", &rebuild)
        .expect("guard");
    assert_eq!(
        verb_args(&guarded)[19],
        "'rename-window' '-t' '%7' '#{@rimz_rename_target}' ; 'set-option' '-w' '-t' '%7' '@rimz_tab_base' 'peer-fast'"
    );
}

#[test]
fn release_clears_ownership_inside_the_observed_name_guard() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let release = backend
        .release_window_command(&pane, "sh", &["%7".to_owned()])
        .expect("release");
    let guarded = backend
        .window_name_guarded_command(&pane, "opus", "sh", &release)
        .expect("guard");
    let command = &verb_args(&guarded)[19];
    assert!(
        command.contains("'set-option' '-wu' '-t' '%7' '@rimz_tab_base'"),
        "{command}"
    );
    assert!(
        command.contains("'set-option' '-wu' '-t' '%7' '@rimz_tab_founders'"),
        "{command}"
    );
}

#[test]
fn projected_rename_runs_under_one_window_name_check() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    for (observed, name) in [
        ("#feat ?", "#feat"),
        ("a_b", "a_b ✓"),
        ("x}y", "{x}"),
        ("it's ⢿", "it's"),
    ] {
        let rename = backend
            .rename_window_command(&pane, name)
            .expect("tmux pane");
        let guarded = backend
            .window_name_guarded_command(&pane, observed, name, &rename)
            .expect("guard");
        let target_name = name.replace([':', '.'], "-");
        assert_eq!(
            verb_args(&guarded),
            [
                "set-option",
                "-w",
                "-t",
                "%7",
                "@rimz_rename_observed",
                observed,
                ";",
                "set-option",
                "-w",
                "-t",
                "%7",
                "@rimz_rename_target",
                target_name.as_str(),
                ";",
                "if-shell",
                "-F",
                "-t",
                "%7",
                "#{==:#{window_name},#{@rimz_rename_observed}}",
                "'rename-window' '-t' '%7' '#{@rimz_rename_target}'",
                ";",
                "set-option",
                "-wu",
                "-t",
                "%7",
                "@rimz_rename_observed",
                ";",
                "set-option",
                "-wu",
                "-t",
                "%7",
                "@rimz_rename_target",
            ],
            "names travel as option values, never as command syntax"
        );
    }
}

#[test]
fn readonly_attach_blocks_input_and_ignores_viewer_size() {
    let spec = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server")
        .attach_readonly_command("rimz-test");

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
fn even_column_heights_reserve_separators_and_distribute_remainder_last() {
    assert_eq!(window::even_column_heights(50, 3), [16, 16, 16]);
    assert_eq!(window::even_column_heights(51, 3), [16, 16, 17]);
    assert_eq!(window::even_column_heights(52, 3), [16, 17, 17]);
}

#[test]
fn even_split_sizes_hand_each_split_the_cells_its_remaining_panes_need() {
    // A chain of splits leaves panes of `even_column_heights`, separators
    // included: 3 columns over 181 cells are 59/60/60, 2 rows over 66 are
    // 32/33, and a run with nothing to split needs no size.
    assert_eq!(window::even_split_sizes(181, 3), [121, 60]);
    assert_eq!(window::even_split_sizes(66, 2), [33]);
    assert_eq!(window::even_split_sizes(50, 3), [33, 16]);
    assert!(window::even_split_sizes(50, 1).is_empty());
    // An extent too small for its panes still returns sizes instead of
    // tripping `even_column_heights`'s debug assertion; tmux clamps a split it
    // cannot fit and errors only once no space is left.
    assert_eq!(window::even_split_sizes(1, 3), [2, 1]);
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
        runtime: crate::disk::paths::RuntimePaths::under(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-empty")),
            tempfile::tempdir().expect("runtime root").path(),
        )
        .expect("runtime paths"),
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
        env: Default::default(),
        title: "work".to_owned(),
        panes: LayoutPanes {
            columns: columns
                .into_iter()
                .map(|panes| LayoutColumn {
                    panes,
                    stacked: false,
                })
                .collect(),
            focused_pane: 0,
        },
        focus: true,
        dock_sidebar: true,
        after: None,
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
    assert!(session_args[5].contains(
        ",#{s/,/_/g:#{?#{@rimz_title},#{@rimz_title},#{pane_title}}},#{pane_floating_flag},"
    ));

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
