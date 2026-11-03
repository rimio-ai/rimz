use super::support::*;

fn status_server(session: &str) -> TmuxServer {
    let server = TmuxServer::new();
    server.ensure_with_shell(session);
    server.output(&["set-option", "-g", "status", "on"]);
    server.output(&["set-option", "-g", "base-index", "0"]);
    server.output(&["move-window", "-r", "-t", session]);
    server
}

fn options(server: &TmuxServer, session: &str, mode: &str) -> SessionOptions {
    let config: rimz::config::MachineConfig =
        toml::from_str(&format!("[mux]\nstatus_bar = {mode:?}\n")).unwrap();
    let mut opts = session_opts(
        session,
        WorkspaceId::from_project_root(server._tempdir.path()),
        server._tempdir.path(),
        server._tempdir.path(),
        Some((120, 40)),
    );
    opts.config = (&config).into();
    opts
}

fn strip(server: &TmuxServer, session: &str) -> String {
    server.stdout(&["show-options", "-qv", "-t", session, "@rimz_tab_strip"])
}

#[track_caller]
fn assert_strip(server: &TmuxServer, session: &str, first: usize, last: usize, current: usize) {
    let strip = strip(server, session);
    let spans: Vec<_> = strip
        .split("#[range=window|")
        .skip(1)
        .map(|span| {
            let (index, rest) = span.split_once(' ').expect("range index ends in a space");
            (
                index.parse::<usize>().expect("numeric index"),
                rest.starts_with("list=focus "),
            )
        })
        .collect();
    assert_eq!(
        spans,
        (first..=last)
            .map(|index| (index, index == current))
            .collect::<Vec<_>>(),
        "{strip}"
    );
}

#[track_caller]
fn assert_inherits_status(server: &TmuxServer, session: &str) {
    let local = server.stdout(&["show-options", "-t", session]);
    assert!(
        !local.lines().any(|line| {
            line.starts_with("status ")
                || line.starts_with("status-format[")
                || line.starts_with("@rimz_status ")
                || line.starts_with("@rimz_tab_strip ")
                || line.starts_with("@rimz_tab_start ")
        }),
        "session must inherit status options: {local}"
    );
}

#[test]
fn compact_strip_tracks_window_events_and_clamps_both_ends() {
    require_tmux!();
    let session = "compact-strip";
    let server = status_server(session);
    server.output(&["set-option", "-gw", "automatic-rename", "off"]);
    server.output(&["set-option", "-gw", "window-status-format", "#I:#W"]);
    server.output(&["set-option", "-gw", "window-status-current-format", "#I:#W"]);
    for index in 1..30 {
        server.output(&[
            "new-window",
            "-d",
            "-t",
            session,
            "-n",
            &format!("w{index}"),
            "sh",
        ]);
    }
    server.output(&["select-window", "-t", &format!("{session}:14")]);
    server
        .backend
        .ensure_session(&options(&server, session, "compact"))
        .unwrap();
    assert_strip(&server, session, 4, 24, 14);
    for (index, first, last) in [(0, 0, 20), (29, 9, 29)] {
        server.output(&["select-window", "-t", &format!("{session}:{index}")]);
        assert_strip(&server, session, first, last, index);
    }
    server.output(&["new-window", "-t", session, "-n", "new", "sh"]);
    assert_strip(&server, session, 10, 30, 30);
    server.output(&[
        "rename-window",
        "-t",
        session,
        "a#{session_name}b #[fg=red]x",
    ]);
    let renamed = strip(&server, session);
    let name = server.display(session, "#{window_name}");
    assert!(renamed.contains(&name), "{renamed}");
    server.output(&["kill-window", "-t", &format!("{session}:30")]);
    assert_strip(&server, session, 9, 29, 29);

    let other = "full-other";
    server
        .backend
        .ensure_session(&options(&server, other, "full"))
        .unwrap();
    server.output(&["new-window", "-t", other, "-n", "other", "sh"]);
    server.output(&["rename-window", "-t", other, "renamed"]);
    assert!(strip(&server, other).is_empty());
    assert_strip(&server, session, 9, 29, 29);

    server.output(&["set-option", "-t", session, "renumber-windows", "off"]);
    server.output(&[
        "move-window",
        "-s",
        &format!("{session}:29"),
        "-t",
        &format!("{session}:50"),
    ]);
    server.output(&["select-window", "-t", &format!("{session}:0")]);
    assert_strip(&server, session, 0, 20, 0);
    server.output(&["select-window", "-t", &format!("{session}:50")]);
    assert_strip(&server, session, 50, 50, 50);
}

#[test]
fn status_modes_restore_inheritance_and_preserve_multiline_formats() {
    require_tmux!();
    let session = "status-modes";
    let server = status_server(session);
    let user_bar = [("status", "off"), ("status-format[3]", "user line")];
    let set_user_bar = || {
        for (key, value) in user_bar {
            server.output(&["set-option", "-t", session, key, value]);
        }
    };
    set_user_bar();
    server
        .backend
        .ensure_session(&options(&server, session, "full"))
        .unwrap();
    for (key, value) in user_bar {
        assert_eq!(server.show_option(&["-t", session], key), value, "{key}");
    }
    server.output(&["set-option", "-u", "-t", session, "status"]);
    server.output(&["set-option", "-u", "-t", session, "status-format"]);
    server
        .backend
        .ensure_session(&options(&server, session, "full"))
        .unwrap();
    assert_inherits_status(&server, session);
    let hooks = server.stdout(&["show-hooks", "-g"]);
    assert!(!hooks.contains("[241]"), "{hooks}");

    // A first off or compact apply, with no marker yet, takes the bar over.
    set_user_bar();
    server
        .backend
        .ensure_session(&options(&server, session, "off"))
        .unwrap();
    assert_eq!(server.show_option(&["-t", session], "status"), "off");
    assert_eq!(server.show_option(&["-t", session], "status-format[3]"), "");
    server.output(&["set-option", "-g", "status", "off"]);
    server
        .backend
        .ensure_session(&options(&server, session, "compact"))
        .unwrap();
    assert_eq!(server.show_option(&["-t", session], "status"), "on");
    assert_strip(&server, session, 0, 0, 0);
    let layout = server.show_option(&["-t", session], "status-format[0]");
    assert!(
        layout.contains("#{@rimz_tab_strip}") && !layout.contains("#{W:"),
        "{layout}"
    );

    let extra = "  #[fg=red,bg=blue] 'literal' #{session_name} %H  ";
    server.output(&["set-option", "-g", "status", "5"]);
    server.output(&["set-option", "-g", "status-format[0]", "custom first line"]);
    server.output(&["set-option", "-g", "status-format[1]", extra]);
    server.output(&["set-option", "-g", "status-format[4]", "fifth line"]);
    server
        .backend
        .ensure_session(&options(&server, session, "compact"))
        .unwrap();
    assert_eq!(
        server
            .output(&["show-options", "-v", "-t", session, "status-format[1]"])
            .stdout,
        format!("{extra}\n").as_bytes()
    );
    assert_eq!(
        server.show_option(&["-t", session], "status-format[4]"),
        "fifth line"
    );
    assert!(
        !server
            .stdout(&["show-options", "-t", session])
            .lines()
            .any(|line| line.starts_with("status "))
    );
    assert_eq!(
        server.show_option(&["-t", session], "status-format[0]"),
        layout
    );

    server
        .backend
        .ensure_session(&options(&server, session, "off"))
        .unwrap();
    assert_eq!(server.show_option(&["-t", session], "status"), "off");
    assert!(strip(&server, session).is_empty());
    server
        .backend
        .ensure_session(&options(&server, session, "full"))
        .unwrap();
    assert_inherits_status(&server, session);
    assert_eq!(
        server.show_option(&["-g"], "status-format[0]"),
        "custom first line"
    );

    set_user_bar();
    server
        .backend
        .ensure_session(&options(&server, session, "compact"))
        .unwrap();
    assert_eq!(server.show_option(&["-t", session], "status"), "");
    assert_eq!(server.show_option(&["-t", session], "status-format[3]"), "");
}

#[test]
fn status_script_failure_restores_inheritance_without_failing_room_open() {
    require_tmux!();
    let session = "status-failure";
    let server = status_server(session);
    server.output(&["set-option", "-g", "status-format[1]", "bad\nscript word"]);
    for (key, value) in [
        ("status", "off"),
        ("status-format[0]", "stale"),
        ("@rimz_status", "compact"),
        ("@rimz_tab_strip", "stale"),
    ] {
        server.output(&["set-option", "-t", session, key, value]);
    }
    let result = server
        .backend
        .ensure_session(&options(&server, session, "compact"));
    assert!(
        result.is_ok(),
        "status failure must not refuse room open: {result:?}"
    );
    assert_inherits_status(&server, session);
}

#[test]
fn background_view_swap_refreshes_compact_strip_in_its_own_session() {
    require_tmux!();
    let session = "status-swap";
    let server = status_server(session);
    server.output(&["new-window", "-d", "-t", session, "-n", "rimzd", "sh"]);
    let active = server.display(session, "#{window_id}");
    server
        .backend
        .ensure_session(&options(&server, session, "compact"))
        .unwrap();
    assert_strip(&server, session, 0, 1, 0);
    if server
        .stdout(&["show-hooks", "-g"])
        .contains("after-swap-window[241]")
    {
        server.output(&["set-hook", "-gu", "after-swap-window[241]"]);
    }
    server.ensure_with_shell("other-current-session");
    let host = rimz::mux::HostPane {
        argv: vec!["sh".into()],
        cwd: server._tempdir.path().into(),
    };
    let view = rimz::mux::BackgroundViewOptions {
        view: rimz::mux::DaemonView {
            name: "rimzd".into(),
            content: vec![host.clone()],
            hosts: vec![],
            loop_panel: host,
        },
        sidebar: sidebar_opts(
            server._tempdir.path(),
            session,
            "/bin/true".into(),
            Some(120),
        ),
    };
    server.backend.open_background_view(&view).unwrap();
    assert_eq!(server.display(session, "#{window_id}"), active);
    assert_eq!(
        server.display(&format!("{session}:0"), "#{window_name}"),
        "rimzd"
    );
    assert_strip(&server, session, 0, 1, 1);
    assert!(strip(&server, "other-current-session").is_empty());
}

#[test]
fn reload_reapplies_room_options_without_starting_stopped_sessions() {
    require_tmux!();
    let env = Env::new();
    env.record(&env.project_root);
    env.record(&env.home_root.join("stopped"));
    let workspace = env.resolve_workspace(&env.project_root);
    let session = &workspace.session_name;
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    server
        .backend
        .ensure_session(&session_opts(
            session,
            workspace.workspace_id.clone(),
            &env.project_root,
            &env.project_root,
            Some((120, 40)),
        ))
        .unwrap();
    server.output(&["set-option", "-g", "status", "on"]);
    server.output(&["set-option", "-g", "base-index", "0"]);
    server.output(&["move-window", "-r", "-t", session]);
    let sessions = server.stdout(&["list-sessions", "-F", "#{session_name}"]);
    for mode in ["off", "compact", "full"] {
        std::fs::write(
            env.rimz_home().join("config.toml"),
            format!("[mux]\nstatus_bar = {mode:?}\n[tmux]\nhistory_limit = 54321\n"),
        )
        .unwrap();
        let result = env.rimz().arg("reload").output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            server.show_option(&["-t", session], "history-limit"),
            "54321"
        );
        match mode {
            "off" => assert_eq!(server.show_option(&["-t", session], "status"), "off"),
            "compact" => assert_strip(&server, session, 0, 0, 0),
            _ => assert_inherits_status(&server, session),
        }
        assert_eq!(
            server.stdout(&["list-sessions", "-F", "#{session_name}"]),
            sessions
        );
    }
    assert!(
        server
            .backend
            .apply_room_options("vanished", &Default::default())
            .is_err()
    );
    assert_eq!(
        server.stdout(&["list-sessions", "-F", "#{session_name}"]),
        sessions
    );
}
