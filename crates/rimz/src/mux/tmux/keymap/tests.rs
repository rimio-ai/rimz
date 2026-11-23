use super::*;
use crate::config::MachineConfig;

#[test]
fn every_named_action_compiles() {
    for name in KeyActionName::vocabulary() {
        let action = KeyAction::Named(KeyActionName::parse(name).expect(name));
        assert_eq!(action.to_string(), name);
        assert!(
            !action_command(&action, Path::new("/bin/rimz"), Some(KeyChord::Ctrl('b'))).is_empty()
        );
    }
}

fn commands(config: &str, roots: &str, prior: Option<&str>) -> Vec<Vec<String>> {
    let mut options = vec![("@rimz_keys", roots), ("mode-keys", "emacs")];
    options.extend(prior.map(|prior| ("@rimz_mode_keys", prior)));
    commands_with(config, &options)
}

fn commands_with(config: &str, options: &[(&str, &str)]) -> Vec<Vec<String>> {
    let config: MachineConfig = toml::from_str(config).unwrap();
    let options: Vec<_> = options
        .iter()
        .map(|(name, value)| words(&[name, value]))
        .collect();
    keymap_commands(&config.room_keymap(), Path::new("/bin/rimz"), &options)
        .into_iter()
        .flatten()
        .collect()
}

fn prefix_commands(commands: &[Vec<String>]) -> Vec<Vec<String>> {
    commands
        .iter()
        .filter(|cmd| cmd[0] == "set-option" && cmd.iter().any(|arg| arg.contains("prefix")))
        .cloned()
        .collect()
}

#[test]
fn a_colliding_tmux_prefix_is_recorded_once_turned_off_and_restored() {
    let ctrl_b = "[room]\nstyle='rimz'\n[keys]\nprefix='Ctrl+b'\n";
    let first = commands_with(ctrl_b, &[("prefix", "C-b"), ("prefix2", "None")]);
    assert_eq!(
        prefix_commands(&first),
        [
            words(&["set-option", "-s", "@rimz_prefix", "C-b"]),
            words(&["set-option", "-g", "prefix", "None"]),
        ]
    );
    let second = commands_with(
        ctrl_b,
        &[
            ("prefix", "None"),
            ("prefix2", "None"),
            ("@rimz_prefix", "C-b"),
        ],
    );
    assert_eq!(prefix_commands(&second), [] as [Vec<String>; 0]);
    let native = commands_with("", &[("prefix", "None"), ("@rimz_prefix", "C-b")]);
    assert_eq!(
        prefix_commands(&native),
        [
            words(&["set-option", "-g", "prefix", "C-b"]),
            words(&["set-option", "-su", "@rimz_prefix"]),
        ]
    );
    let rimz = commands_with(
        "[room]\nstyle='rimz'",
        &[("prefix", "C-b"), ("prefix2", "M-y")],
    );
    assert_eq!(prefix_commands(&rimz), [] as [Vec<String>; 0]);
    let second_prefix = commands_with(ctrl_b, &[("prefix", "C-a"), ("prefix2", "C-b")]);
    assert_eq!(
        prefix_commands(&second_prefix),
        [
            words(&["set-option", "-s", "@rimz_prefix2", "C-b"]),
            words(&["set-option", "-g", "prefix2", "None"]),
        ]
    );
}

#[test]
fn native_binds_only_sidebar_commands_and_removes_recorded_keys() {
    let commands = commands("", "M-x M-; M-?", None);
    for key in ["M-x", "M-;", "M-?"] {
        assert!(
            commands.contains(
                &["unbind-key", "-T", "root", key]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
    }
    let binds: Vec<_> = commands.iter().filter(|cmd| cmd[0] == "bind-key").collect();
    assert_eq!(binds.len(), 2);
    assert!(
        binds
            .iter()
            .any(|cmd| cmd[3] == "M-p"
                && cmd.last().unwrap().contains("'sidebar' 'focus' '--toggle'"))
    );
    assert!(
        binds
            .iter()
            .any(|cmd| cmd[3] == "M-g" && cmd.last().unwrap().contains("'pane' 'zoom'"))
    );
    assert!(
        !commands
            .iter()
            .any(|cmd| cmd.iter().any(|arg| arg == "mode-keys"))
    );
}

#[test]
fn rimz_compiles_native_actions_sticky_tables_and_uppercase_aliases() {
    let commands = commands("[room]\nstyle='rimz'", "", None);
    let bind = |table: &str, key: &str| {
        commands
            .iter()
            .find(|cmd| cmd[0] == "bind-key" && cmd[2] == table && cmd[3] == key)
            .unwrap()
    };
    assert_eq!(&bind("root", "M-h")[4..], ["select-pane", "-L"]);
    assert_eq!(
        &bind("root", "M-t")[4..],
        ["new-window", "-c", "#{pane_current_path}"]
    );
    assert_eq!(
        &bind("rimz-resize", "h")[4..],
        [
            "resize-pane",
            "-L",
            ";",
            "switch-client",
            "-T",
            "rimz-resize"
        ]
    );
    assert_eq!(
        &bind("rimz-prefix", "r")[4..],
        ["switch-client", "-T", "rimz-resize"]
    );
    assert_eq!(&bind("root", "M-J")[4..], &bind("root", "M-S-J")[4..]);
    for key in ["H", "J", "K", "L", "X"] {
        assert!(
            commands.iter().any(|command| command[0] == "bind-key"
                && command[2] == "rimz-prefix"
                && command[3] == format!("S-{key}")),
            "extended shifted {key} must work on tmux 3.5"
        );
        assert_eq!(
            &bind("rimz-prefix", key)[4..],
            &bind("rimz-prefix", &format!("S-{key}"))[4..],
        );
    }
    assert_eq!(&bind("root", "M-?")[4..], &bind("root", "M-S-?")[4..]);
    assert!(
        commands.contains(
            &["set-option", "-gw", "mode-keys", "vi"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    assert!(
        commands.contains(
            &["set-option", "-s", "@rimz_mode_keys", "emacs"]
                .map(str::to_owned)
                .to_vec()
        )
    );
}

fn bind<'a>(commands: &'a [Vec<String>], table: &str, key: &str) -> &'a [String] {
    let bind = commands
        .iter()
        .find(|cmd| cmd[0] == "bind-key" && cmd[2] == table && cmd[3] == key);
    assert!(bind.is_some(), "{table} {key} must be bound");
    &bind.unwrap()[4..]
}

#[test]
fn sticky_modes_reenter_only_after_a_motion_and_prefix_stays_one_shot() {
    let commands = commands(
        "[room]\nstyle='rimz'\n[keys.bind.pane]\nh='pane.focus.left'\nz='pane.zoom'\n\
         [keys.bind.tab]\nn='tab.next'\nc='tab.new'\n[keys.bind.resize]\nz='pane.zoom'\n\
         [keys.bind.move]\nz='pane.zoom'\n",
        "",
        None,
    );
    for table in [
        "rimz-prefix",
        "rimz-pane",
        "rimz-tab",
        "rimz-resize",
        "rimz-move",
    ] {
        assert!(
            commands.contains(&words(&["unbind-key", "-a", "-T", table])),
            "{table} must be cleared on every apply"
        );
    }
    let sticky = |table: &str| words(&[";", "switch-client", "-T", table]);
    assert_eq!(
        bind(&commands, "rimz-pane", "h"),
        [words(&["select-pane", "-L"]), sticky("rimz-pane")].concat()
    );
    assert_eq!(
        bind(&commands, "rimz-tab", "n"),
        [words(&["next-window"]), sticky("rimz-tab")].concat()
    );
    let move_h = bind(&commands, "rimz-move", "h");
    assert_eq!(move_h[0], "if-shell");
    assert_eq!(move_h[5..], sticky("rimz-move"));
    for (table, key) in [
        ("rimz-pane", "z"),
        ("rimz-tab", "c"),
        ("rimz-resize", "z"),
        ("rimz-move", "z"),
        ("rimz-prefix", "h"),
    ] {
        assert!(
            !bind(&commands, table, key).contains(&";".to_owned()),
            "{table} {key} must return to normal"
        );
    }
}

/// tmux's own parse of one nested command string.
fn parsed(command: &str) -> Vec<String> {
    shlex::split(command).unwrap()
}

#[test]
fn split_close_and_swap_leave_the_sidebar_alone_in_every_bundled_keymap() {
    let on_sidebar =
        "#{||:#{==:#{pane_title},rimz-sidebar},#{m:* sidebar serve*,#{pane_start_command}}}";
    for keymap in ["rimz", "tmux", "zellij", "vim"] {
        let config = format!("[room]\nstyle='rimz'\n[keys]\nkeymap='{keymap}'\n");
        let commands = commands(&config, "", None);
        let machine: MachineConfig = toml::from_str(&config).unwrap();
        let mut checked = 0;
        for (&mode, bindings) in &machine.room_keymap().modes {
            for (&key, action) in bindings {
                let KeyAction::Named(
                    name @ (KeyActionName::PaneClose
                    | KeyActionName::PaneSplit(_)
                    | KeyActionName::PaneSwap(_)),
                ) = action
                else {
                    continue;
                };
                let body = bind(&commands, table(mode), &keys(key)[0]);
                assert_eq!(
                    body[..3],
                    words(&["if-shell", "-F", on_sidebar]),
                    "{keymap} {key:?}"
                );
                match *name {
                    KeyActionName::PaneClose => {
                        assert_eq!(body[3..5], words(&["", "kill-pane"]))
                    }
                    KeyActionName::PaneSplit(direction) => {
                        let (side, flag) = match direction {
                            KeySplitDirection::Right => ("right", "-h"),
                            KeySplitDirection::Down => ("down", "-v"),
                        };
                        let child = parsed(&body[3]);
                        assert_eq!(child[..2], words(&["run-shell", "-b"]));
                        assert_eq!(
                            parsed(&child[2]),
                            words(&[
                                "/bin/rimz",
                                "--mux",
                                "tmux",
                                "sidebar",
                                "split",
                                side,
                                "--from-pane",
                                "tmux:#{pane_id}",
                                "--session-name",
                                "#{session_name}",
                            ])
                        );
                        assert_eq!(
                            parsed(&body[4]),
                            words(&["split-window", flag, "-c", "#{pane_current_path}"])
                        );
                    }
                    KeyActionName::PaneSwap(direction) => {
                        let target = format!("{{{direction}-of}}");
                        assert_eq!(body[3], "");
                        let inner = parsed(&body[4]);
                        assert_eq!(
                            inner[..6],
                            words(&[
                                "if-shell",
                                "-F",
                                "-t",
                                &target,
                                &format!("#{{&&:#{{pane_format}},{on_sidebar}}}"),
                                "",
                            ])
                        );
                        assert_eq!(parsed(&inner[6]), words(&["swap-pane", "-s", &target]));
                    }
                    _ => unreachable!("filtered above"),
                }
                checked += 1;
            }
        }
        assert!(checked >= 7, "{keymap}: close, both splits and four swaps");
    }
}

#[test]
fn card_close_runs_the_close_of_the_pressed_pane_in_every_bundled_keymap_and_a_user_bind() {
    let user = "[room]\nstyle='rimz'\n[keys.bind.normal]\n'Alt+;'='card.close'\n";
    let bundled = ["rimz", "tmux", "zellij", "vim"]
        .map(|keymap| format!("[room]\nstyle='rimz'\n[keys]\nkeymap='{keymap}'\n"));
    for config in bundled.iter().map(String::as_str).chain([user]) {
        let commands = commands(config, "", None);
        let machine: MachineConfig = toml::from_str(config).unwrap();
        let mut checked = 0;
        for (&mode, bindings) in &machine.room_keymap().modes {
            for (&key, action) in bindings {
                if *action != KeyAction::Named(KeyActionName::CardClose) {
                    continue;
                }
                let body = bind(&commands, table(mode), &keys(key)[0]);
                assert_eq!(body[..2], words(&["run-shell", "-b"]), "{config} {key:?}");
                assert_eq!(
                    parsed(&body[2]),
                    words(&[
                        "/bin/rimz",
                        "--mux",
                        "tmux",
                        "sidebar",
                        "close",
                        "--from-pane",
                        "tmux:#{pane_id}",
                        "--session-name",
                        "#{session_name}",
                    ]),
                    "{config} {key:?}"
                );
                checked += 1;
            }
        }
        assert!(checked >= 1, "{config}: card.close must be bound");
    }
}

#[test]
fn send_prefix_focus_next_and_arrow_keys_compile() {
    for (prefix, sent) in [("Ctrl+b", "C-b"), ("Alt+w", "M-w"), ("Alt+;", "M-\\;")] {
        let commands = commands(
            &format!(
                "[room]\nstyle='rimz'\n[keys]\nprefix='{prefix}'\n[keys.bind.prefix]\n\
                 b='pane.send_prefix'\no='pane.focus.next'\nLeft='pane.focus.left'\n\
                 'Alt+Up'='pane.focus.up'\n'Ctrl+Down'='pane.focus.down'\n"
            ),
            "",
            None,
        );
        assert_eq!(bind(&commands, "rimz-prefix", "b"), ["send-keys", sent]);
        assert_eq!(
            bind(&commands, "rimz-prefix", "o"),
            ["select-pane", "-t", ":.+"]
        );
        assert_eq!(
            bind(&commands, "rimz-prefix", "Left")[..2],
            ["select-pane", "-L"]
        );
        assert_eq!(
            bind(&commands, "rimz-prefix", "M-Up")[..2],
            ["select-pane", "-U"]
        );
        assert_eq!(
            bind(&commands, "rimz-prefix", "C-Down")[..2],
            ["select-pane", "-D"]
        );
    }
}

#[test]
fn native_scrollback_leaves_mode_keys_and_restores_a_recorded_value() {
    let config: MachineConfig = toml::from_str("[room]\nstyle='rimz'").unwrap();
    let mut keymap = config.room_keymap();
    keymap.style.as_mut().unwrap().scrollback = KeyScrollback::Native;
    let run = |options: &[(&str, &str)]| -> Vec<Vec<String>> {
        let options: Vec<_> = options
            .iter()
            .map(|(name, value)| words(&[name, value]))
            .collect();
        keymap_commands(&keymap, Path::new("/bin/rimz"), &options)
            .into_iter()
            .flatten()
            .filter(|cmd| {
                cmd.iter()
                    .any(|arg| arg.contains("mode-keys") || arg.contains("mode_keys"))
            })
            .collect()
    };
    assert_eq!(run(&[("mode-keys", "emacs")]), [] as [Vec<String>; 0]);
    assert_eq!(
        run(&[("mode-keys", "vi"), ("@rimz_mode_keys", "emacs")]),
        [
            words(&["set-option", "-gw", "mode-keys", "emacs"]),
            words(&["set-option", "-su", "@rimz_mode_keys"]),
        ]
    );
}

#[test]
fn native_restores_mode_keys_only_behind_the_marker() {
    let commands = commands("", "", Some("emacs"));
    assert!(
        commands.contains(
            &["set-option", "-gw", "mode-keys", "emacs"]
                .map(str::to_owned)
                .to_vec()
        )
    );
    assert!(
        commands.contains(
            &["set-option", "-su", "@rimz_mode_keys"]
                .map(str::to_owned)
                .to_vec()
        )
    );
}

#[test]
fn user_commands_quote_literal_formats_and_do_not_append_session_name() {
    let commands = commands(
        "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+y\"={run=\"rimz message @coder -- '#{session_name} % ;'\"}",
        "",
        None,
    );
    let bind = commands
        .iter()
        .find(|cmd| cmd[0] == "bind-key" && cmd[3] == "M-y")
        .unwrap();
    assert_eq!(
        &bind[4..],
        [
            "run-shell",
            "-b",
            "cd -- #{q:session_path} && '/bin/rimz' '--mux' 'tmux' 'message' '@coder' \
             '--' '##{session_name} % ;'"
        ]
    );
    let palette = commands
        .iter()
        .find(|cmd| cmd[0] == "bind-key" && cmd[3] == "M-n")
        .unwrap();
    assert!(
        palette
            .last()
            .unwrap()
            .contains("'--client' '#{client_name}' '--from-pane' 'tmux:#{pane_id}'")
    );
    assert!(
        palette
            .last()
            .unwrap()
            .contains("'--session-name' '#{session_name}'")
    );
}

#[test]
fn alt_digits_run_the_tab_verbs_and_an_overlay_restores_a_slot() {
    let bind = |commands: &[Vec<String>], key: &str| {
        commands
            .iter()
            .find(|cmd| cmd[0] == "bind-key" && cmd[3] == key)
            .map(|cmd| cmd[4..].to_vec())
    };
    let bundled = commands("[room]\nstyle='rimz'", "", None);
    for (key, verb) in [
        ("M-3", "'tab' 'terminal' '3'"),
        ("M-0", "'tab' 'dashboard'"),
    ] {
        assert_eq!(
            bind(&bundled, key).unwrap(),
            [
                "run-shell".to_owned(),
                "-b".to_owned(),
                format!("'/bin/rimz' '--mux' 'tmux' {verb} '--session-name' '#{{session_name}}'")
            ]
        );
    }
    let overlay = commands(
        "[room]\nstyle='rimz'\n[keys.bind.normal]\n'Alt+3'='card.slot.3'\n'Alt+0'='none'",
        "",
        None,
    );
    assert!(
        bind(&overlay, "M-3")
            .unwrap()
            .last()
            .unwrap()
            .contains("'sidebar' 'slot' '3'")
    );
    assert_eq!(bind(&overlay, "M-0"), None);
}

#[test]
fn named_commands_do_not_depend_on_the_session_start_directory() {
    for config in ["", "[room]\nstyle='rimz'"] {
        for command in commands(config, "", None) {
            if command[0] == "bind-key" && command.get(4).is_some_and(|word| word == "run-shell") {
                assert!(
                    command[6].starts_with("'/bin/rimz' '--mux' 'tmux' "),
                    "named actions must run even after the start directory is removed: {command:?}"
                );
            }
        }
    }
}
