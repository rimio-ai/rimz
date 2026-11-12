use super::support::*;

fn apply(server: &TmuxServer, config: &str, bin: &Path) {
    let config: rimz::config::MachineConfig = toml::from_str(config).unwrap();
    server
        .backend
        .apply_room_keymap(&config.room_keymap(), bin)
        .unwrap();
}

fn has_key(keys: &str, key: &str) -> bool {
    keys.lines()
        .filter_map(shlex::split)
        .any(|words| words.iter().any(|word| word == key))
}

#[test]
fn apply_removes_owned_keys_restores_mode_keys_and_preserves_foreign_bindings() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("keys");
    let bin = Path::new("/bin/rimz");
    server.output(&["set-option", "-gw", "mode-keys", "vi"]);
    for key in ["S-Enter", "M-Enter", "User240"] {
        server.output(&["bind-key", "-n", key, "display-message", "kept"]);
    }
    let mouse: Vec<_> = server
        .list_keys("root")
        .lines()
        .filter(|line| line.contains("Mouse"))
        .map(str::to_owned)
        .collect();
    apply(&server, "[sidebar]\nfocus_key='Alt+x'", bin);
    assert!(has_key(&server.list_keys("root"), "M-x"));
    assert_eq!(server.show_option(&["-gw"], "mode-keys"), "vi");
    server.output(&["set-option", "-gw", "mode-keys", "emacs"]);
    apply(
        &server,
        "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+;\"='card.close'\n\"Alt+?\"='card.reopen'",
        bin,
    );
    let root = server.list_keys("root");
    assert!(!has_key(&root, "M-x"));
    for key in ["M-J", "M-S-J", "M-;", "M-?"] {
        assert!(has_key(&root, key), "{root}");
    }
    assert_eq!(server.show_option(&["-gw"], "mode-keys"), "vi");
    assert!(has_key(&server.list_keys("rimz-prefix"), "r"));
    apply(&server, "[room]\nstyle='rimz'\n[keys]\nkeymap='tmux'", bin);
    assert_eq!(server.show_option(&["-gw"], "mode-keys"), "emacs");
    assert!(has_key(&server.list_keys("rimz-prefix"), "%"));
    apply(&server, "[sidebar]\nfocus_key='Alt+y'", bin);
    let root = server.list_keys("root");
    for key in ["M-x", "M-j", "M-;", "M-?", "M-S-J"] {
        assert!(!has_key(&root, key), "{root}");
    }
    for key in ["M-y", "M-g", "S-Enter", "M-Enter", "User240"] {
        assert!(has_key(&root, key), "{root}");
    }
    for binding in mouse {
        assert!(root.contains(&binding));
    }
    for table in ["rimz-prefix", "rimz-resize", "rimz-move"] {
        assert!(!server.stdout(&["list-keys"]).contains(table));
    }
    assert_eq!(server.show_option(&["-gw"], "mode-keys"), "emacs");
}

fn wait_for(server: &TmuxServer, format: &str, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let value = server.stdout(&["list-clients", "-F", format]);
        if value.trim() == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "expected {expected:?}, got {value:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn pty_prefix_resize_unbound_and_copy_mode_follow_the_compiled_tables() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("keys");
    apply(&server, "[room]\nstyle='rimz'", Path::new("/bin/rimz"));
    server.output(&["split-window", "-h", "-t", "keys", "-c", "/", "sh"]);
    let mut client = AttachedTmuxClient::attach(&server.socket, "keys", 120, 40);
    wait_for(&server, "#{client_key_table}", "root");
    let focused = server.display("keys", "#{pane_id}");
    client.send(b"\x1bpH");
    wait_for(&server, "#{pane_id}|#{pane_left}", &format!("{focused}|0"));
    client.send(b"\x1bp");
    wait_for(&server, "#{client_key_table}", "rimz-prefix");
    client.send(b"r");
    wait_for(&server, "#{client_key_table}", "rimz-resize");
    client.send(b"h");
    thread::sleep(Duration::from_millis(1200));
    wait_for(&server, "#{client_key_table}", "rimz-resize");
    client.send(b"\x1b");
    wait_for(&server, "#{client_key_table}", "root");
    client.send(b"\x1bp");
    wait_for(&server, "#{client_key_table}", "rimz-prefix");
    client.send(b"y");
    wait_for(&server, "#{client_key_table}", "root");
    client.send(b"\x1bp");
    wait_for(&server, "#{client_key_table}", "rimz-prefix");
    client.send(b"[");
    wait_for(
        &server,
        "#{client_key_table}|#{pane_mode}",
        "root|copy-mode",
    );
}

#[test]
fn pty_commands_resolve_the_pressing_room_and_preserve_literal_arguments() {
    use std::os::unix::fs::PermissionsExt;

    require_tmux!();
    let server = TmuxServer::new();
    let first = server._tempdir.path().join("first");
    let second = server._tempdir.path().join("second ' #$ room");
    for (session, dir) in [("first", &first), ("second", &second)] {
        std::fs::create_dir(dir).unwrap();
        server.output(&[
            "new-session",
            "-d",
            "-s",
            session,
            "-c",
            dir.to_str().unwrap(),
            "sh",
        ]);
        server.output(&[
            "set-environment",
            "-t",
            session,
            rimz::workspace::ENV_WORKSPACE_ID,
            WorkspaceId::from_project_root(dir).as_str(),
        ]);
    }
    let bin = server._tempdir.path().join("rimz # stub");
    std::fs::write(
        &bin,
        "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$RIMZ_WORKSPACE_ID\" \"$@\" > \"$PWD/pressed\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    apply(
        &server,
        "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+y\"={run=\"rimz message @coder '#{session_name} % ;'\"}",
        &bin,
    );
    let mut client = AttachedTmuxClient::attach(&server.socket, "second", 120, 40);
    wait_for(&server, "#{client_key_table}", "root");
    let pressed = second.join("pressed");
    for (bytes, expected) in [
        (
            &b"\x1bpz"[..],
            "--mux\ntmux\npane\nzoom\n--session-name\nsecond\n",
        ),
        (
            &b"\x1by"[..],
            "--mux\ntmux\nmessage\n@coder\n#{session_name} % ;\n",
        ),
    ] {
        client.send(bytes);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = std::fs::read_to_string(&pressed).unwrap_or_default();
            if output.ends_with(expected) {
                assert!(
                    output.starts_with(&format!(
                        "{}\n{}\n",
                        second.display(),
                        WorkspaceId::from_project_root(&second)
                    )),
                    "{output}"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "expected {expected:?}, got {output:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
        std::fs::remove_file(&pressed).unwrap();
    }
    assert!(!first.join("pressed").exists());
}

#[test]
fn pty_a_colliding_tmux_prefix_reaches_rimz_and_is_restored() {
    require_tmux!();
    let server = TmuxServer::new();
    let received = server._tempdir.path().join("received");
    let reader = format!(
        "stty raw -echo; dd bs=1 count=1 2>/dev/null | od -An -tx1 > '{}'; sleep 600",
        received.display()
    );
    server.output(&["new-session", "-d", "-s", "keys", "sh", "-c", &reader]);
    server.output(&["set-option", "-g", "prefix", "C-b"]);
    let bin = Path::new("/bin/rimz");
    apply(&server, "[room]\nstyle='rimz'\n[keys]\nkeymap='tmux'", bin);
    assert_eq!(server.show_option(&["-g"], "prefix"), "None");
    assert_eq!(server.show_option(&["-s"], "@rimz_prefix"), "C-b");
    let mut client = AttachedTmuxClient::attach(&server.socket, "keys", 120, 40);
    wait_for(&server, "#{client_key_table}", "root");
    client.send(b"\x02");
    wait_for(&server, "#{client_key_table}", "rimz-prefix");
    client.send(b"\x02");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let bytes = std::fs::read_to_string(&received).unwrap_or_default();
        if bytes.trim() == "02" {
            break;
        }
        assert!(Instant::now() < deadline, "the pane read {bytes:?}");
        thread::sleep(Duration::from_millis(20));
    }
    apply(&server, "", bin);
    assert_eq!(server.show_option(&["-g"], "prefix"), "C-b");
    assert!(
        !server
            .stdout(&["show-options", "-s"])
            .contains("@rimz_prefix")
    );
}

fn poll(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// `pane_id left` for each pane of `target`'s window, in tmux's order.
fn layout(server: &TmuxServer, target: &str) -> String {
    server.stdout(&[
        "list-panes",
        "-t",
        target,
        "-F",
        "#{pane_id} #{pane_left} #{pane_top}",
    ])
}

/// A sidebar titled as the room titles it, with work panes `A` and `B` to its right.
fn sidebar_window(server: &TmuxServer, session: &str) -> [String; 3] {
    server.ensure_with_shell(session);
    let sidebar = server.display(session, "#{pane_id}");
    server.tmux(&["select-pane", "-t", &sidebar, "-T", "rimz-sidebar"]);
    server.tmux(&["split-window", "-h", "-t", &sidebar, "-c", "/", "sh"]);
    let a = server.display(session, "#{pane_id}");
    server.tmux(&["split-window", "-h", "-t", &a, "-c", "/", "sh"]);
    let b = server.display(session, "#{pane_id}");
    [sidebar, a, b]
}

#[test]
fn pty_split_close_and_swap_never_act_on_the_sidebar() {
    use std::os::unix::fs::PermissionsExt;

    require_tmux!();
    let server = TmuxServer::new();
    let [sidebar, a, b] = sidebar_window(&server, "keys");
    server.ensure_with_shell("other");
    let pressed = server._tempdir.path().join("pressed");
    let bin = server._tempdir.path().join("rimz-stub");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
            pressed.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    apply(
        &server,
        "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+m\"='mode.move'",
        &bin,
    );
    let mut client = AttachedTmuxClient::attach(&server.socket, "keys", 160, 40);
    let mut other = AttachedTmuxClient::attach(&server.socket, "other", 160, 40);
    let table = |server: &TmuxServer| {
        server.stdout(&["list-clients", "-t", "keys", "-F", "#{client_key_table}"])
    };
    poll("both clients", || {
        server
            .stdout(&["list-clients", "-F", "#{session_name}"])
            .lines()
            .count()
            == 2
    });
    let before = layout(&server, "keys");

    // Split on the sidebar runs `sidebar split` for the pressing session, even
    // when another session's client was active last.
    server.tmux(&["select-pane", "-t", &sidebar]);
    other.send(b"x");
    thread::sleep(Duration::from_millis(50));
    client.send(b"\x1bps");
    poll("the sidebar split command", || pressed.exists());
    poll("the whole argv", || {
        std::fs::read_to_string(&pressed)
            .unwrap_or_default()
            .ends_with("keys\n")
    });
    assert_eq!(
        std::fs::read_to_string(&pressed).unwrap(),
        format!(
            "--mux\ntmux\nsidebar\nsplit\ndown\n--from-pane\ntmux:{sidebar}\n--session-name\nkeys\n"
        )
    );
    std::fs::remove_file(&pressed).unwrap();

    // Close and swap on the sidebar change nothing; move mode stays entered.
    client.send(b"\x1bpX");
    // tmux 3.5 reads an Alt chord written right behind another key as Esc and the key.
    thread::sleep(Duration::from_millis(300));
    client.send(b"\x1bm");
    poll("move mode", || table(&server).trim() == "rimz-move");
    client.send(b"l");
    client.send(b"h");
    thread::sleep(Duration::from_millis(600));
    assert_eq!(table(&server).trim(), "rimz-move");
    assert_eq!(layout(&server, "keys"), before);
    assert_eq!(server.display("keys", "#{pane_id}"), sidebar);

    // Beside the sidebar: left reaches it and does nothing; right swaps A and B;
    // right again from the rightmost pane wraps to the sidebar and does nothing.
    server.tmux(&["select-pane", "-t", &a]);
    client.send(b"h");
    thread::sleep(Duration::from_millis(600));
    assert_eq!(layout(&server, "keys"), before);
    client.send(b"l");
    poll("A swapped right", || {
        let left = |pane: &str| server.display(pane, "#{pane_left}").parse::<u32>().unwrap();
        left(&a) > left(&b)
    });
    let swapped = layout(&server, "keys");
    assert_eq!(server.display("keys", "#{pane_id}"), a);
    client.send(b"l");
    thread::sleep(Duration::from_millis(600));
    assert_eq!(layout(&server, "keys"), swapped);
    assert_eq!(table(&server).trim(), "rimz-move");

    // No neighbour above: tmux's own error, as the bare swap gives.
    client.send(b"k");
    poll("tmux's missing-pane error", || {
        client.screen().contains("find pane")
    });
    client.send(b"\x1b");
    poll("normal mode", || table(&server).trim() == "root");

    // On a work pane, split and close are the bare commands.
    client.send(b"\x1bps");
    poll("a native split below A", || {
        layout(&server, "keys").lines().count() == 4
    });
    let below = server.display("keys", "#{pane_id}");
    assert!(![&sidebar, &a, &b].contains(&&below));
    client.send(b"\x1bpX");
    poll("the split pane closed", || {
        layout(&server, "keys") == swapped
    });
    assert!(!pressed.exists(), "a work pane starts no rimz process");
}

#[test]
fn pty_split_on_the_sidebar_splits_the_remembered_work_pane_in_its_directory() {
    require_tmux!();
    let env = Env::new();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    env.rimz()
        .args(["--mux", "tmux", "start", "--no-attach"])
        .assert_success_within_timeout("start the split room");
    let panes = list_session_panes(&server, &session);
    let sidebar = panes
        .iter()
        .find(|pane| pane.is_rimz_sidebar())
        .expect("a sidebar");
    let window = server.display(sidebar.pane_id.raw(), "#{window_id}");
    let first = server.stdout(&["list-panes", "-t", &window, "-F", "#{pane_id}"]);
    let first = first
        .lines()
        .find(|pane| *pane != sidebar.pane_id.raw())
        .expect("a work pane beside the sidebar");
    let dir = env.project_root.join("second");
    std::fs::create_dir_all(&dir).unwrap();
    let second = server.stdout(&[
        "split-window",
        "-v",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        first,
        "-c",
        dir.to_str().unwrap(),
        "sh",
    ]);
    let second = second.trim();
    let config: rimz::config::MachineConfig = toml::from_str("[room]\nstyle='rimz'").unwrap();
    server
        .backend
        .apply_room_keymap(&config.room_keymap(), &env.rimz_bin())
        .unwrap();
    let mut client = AttachedTmuxClient::attach(&server.socket, &session, 160, 50);
    let frame = env.runtime_paths().pane_frame_path();
    let remembered = format!("tmux:{second}");
    let deadline = Instant::now() + Duration::from_secs(10);
    server.tmux(&["select-window", "-t", &window]);
    loop {
        server.tmux(&["select-pane", "-t", second]);
        let published = std::fs::read(&frame)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let tabs = published.as_ref().map(|frame| frame["tabs"].clone());
        if tabs
            .as_ref()
            .and_then(|tabs| tabs.as_array())
            .is_some_and(|tabs| {
                tabs.iter()
                    .any(|tab| tab["work_pane"].as_str() == Some(&remembered))
            })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the sidebar must remember {remembered}: {tabs:#?}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    server.tmux(&["select-pane", "-t", sidebar.pane_id.raw()]);
    let before = layout(&server, sidebar.pane_id.raw());
    client.send(b"\x1bps");
    poll("a pane split below the second work pane", || {
        layout(&server, sidebar.pane_id.raw()).lines().count() == before.lines().count() + 1
    });
    let created = server.display(&session, "#{pane_id}");
    assert!(
        !before.contains(&format!("{created} ")),
        "focus lands in the new pane"
    );
    assert_eq!(
        server.display(&created, "#{pane_left}"),
        server.display(second, "#{pane_left}")
    );
    assert!(
        server
            .display(&created, "#{pane_top}")
            .parse::<u32>()
            .unwrap()
            > server
                .display(second, "#{pane_top}")
                .parse::<u32>()
                .unwrap()
    );
    poll("the new pane's shell in the work pane's directory", || {
        server.display(&created, "#{pane_current_path}") == dir.to_str().unwrap()
    });

    // A window whose only pane is the sidebar: split changes nothing.
    use std::os::unix::fs::PermissionsExt;
    let scratch = tempfile::tempdir().unwrap();
    let (stderr, done) = (scratch.path().join("stderr"), scratch.path().join("done"));
    let bin = scratch.path().join("rimz");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\n'{}' \"$@\" 2>'{}'\necho $? >'{}'\n",
            env.rimz_bin().display(),
            stderr.display(),
            done.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    server
        .backend
        .apply_room_keymap(&config.room_keymap(), &bin)
        .unwrap();
    let lone = server.stdout(&[
        "new-window",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{session}:"),
        "sh",
    ]);
    let lone = lone.trim();
    let lone_window = server.display(lone, "#{window_id}");
    // Let the room dock its own sidebar, then leave a sidebar-titled pane alone.
    thread::sleep(Duration::from_millis(1_000));
    for pane in layout(&server, &lone_window).lines() {
        let pane = pane.split(' ').next().unwrap();
        if pane != lone {
            server.tmux(&["kill-pane", "-t", pane]);
        }
    }
    server.tmux(&["select-pane", "-t", lone, "-T", "rimz-sidebar"]);
    server.tmux(&["select-window", "-t", &lone_window]);
    server.tmux(&["select-pane", "-t", lone]);
    let windows = || server.stdout(&["list-windows", "-t", &session, "-F", "#{window_id}"]);
    let (before, before_windows) = (layout(&server, &lone_window), windows());
    assert_eq!(before.lines().count(), 1, "{before}");
    client.send(b"\x1bps");
    poll("the split command to finish", || done.exists());
    poll("its exit status", || {
        std::fs::read_to_string(&done).is_ok_and(|rc| rc.ends_with('\n'))
    });
    assert_eq!(std::fs::read_to_string(&done).unwrap(), "0\n");
    assert_eq!(
        std::fs::read_to_string(&stderr).unwrap().trim(),
        "rimz: sidebar is the only pane in its view; nothing to split"
    );
    assert_eq!(layout(&server, &lone_window), before);
    assert_eq!(windows(), before_windows);
}
