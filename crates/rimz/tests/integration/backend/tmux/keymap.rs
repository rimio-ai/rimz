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
    apply(
        &server,
        "[room]\nstyle='rimz'\n[keys]\nprefix='Ctrl+b'\n[keys.bind.prefix]\nb='pane.send_prefix'",
        bin,
    );
    assert_eq!(server.show_option(&["-g"], "prefix"), "None");
    assert_eq!(server.show_option(&["-s"], "@rimz_prefix"), "C-b");
    let mut client = AttachedTmuxClient::attach(&server.socket, "keys", 120, 40);
    wait_for(&server, "#{client_key_table}", "root");
    client.send(b"\x02");
    wait_for(&server, "#{client_key_table}", "rimz-prefix");
    client.send(b"b");
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
