use super::*;

fn commands(mode: MuxStatusBar) -> Vec<Vec<String>> {
    status_commands("room", mode, "on", &std::array::from_fn(|_| None), false)
}

#[test]
fn status_modes_clear_owned_state_before_writing_their_mode() {
    let modes = [MuxStatusBar::Full, MuxStatusBar::Compact, MuxStatusBar::Off];
    let outcomes = modes.map(|mode| {
        let commands = commands(mode);
        let unset = |key: &'static str| ["set-option", "-qu", "-t", "room", key];
        let gated_bar = commands[0]
            == [
                "if-shell",
                "-F",
                "-t",
                "room",
                "#{@rimz_status}",
                "'set-option' '-qu' '-t' 'room' 'status' ; \
                 'set-option' '-qu' '-t' 'room' 'status-format'",
            ];
        let plain_bar = commands[..2] == [unset("status"), unset("status-format")];
        let cleared = ["@rimz_status", "@rimz_tab_strip", "@rimz_tab_start"]
            .into_iter()
            .all(|key| commands.iter().any(|command| command == &unset(key)));
        let marker = commands
            .iter()
            .find(|command| command[..4] == ["set-option", "-t", "room", "@rimz_status"])
            .map(|command| command[4].clone());
        let disabled = commands
            .iter()
            .any(|command| command == &["set-option", "-t", "room", "status", "off"]);
        (
            match (gated_bar, plain_bar) {
                (true, false) => "gated",
                (false, true) => "always",
                _ => "missing",
            },
            cleared,
            marker,
            disabled,
            commands.iter().any(|command| command[0] == "set-hook"),
        )
    });
    assert_eq!(
        outcomes,
        [
            ("gated", true, None, false, false),
            ("always", true, Some("compact".into()), false, true),
            ("always", true, Some("off".into()), true, false)
        ]
    );
}

#[test]
fn compact_preserves_global_extra_lines_and_only_enables_a_disabled_bar() {
    let formats = [
        Some("  #[fg=red,bg=blue] 'quoted' #{session_name}  ".into()),
        None,
        Some(String::new()),
        Some("%H } ; #W".into()),
    ];
    let outcomes = ["off", "on", "2", "5"].map(|global| {
        let commands = status_commands("room", MuxStatusBar::Compact, global, &formats, false);
        let lines: Vec<_> = commands
            .iter()
            .filter(|command| {
                command.first().is_some_and(|word| word == "set-option")
                    && command
                        .get(3)
                        .is_some_and(|word| word.starts_with("status-format["))
            })
            .map(|command| (command[3].clone(), command[4].clone()))
            .collect();
        let enabled = commands
            .iter()
            .any(|command| command == &["set-option", "-t", "room", "status", "on"]);
        (lines, enabled)
    });
    let lines = vec![
        ("status-format[0]".into(), COMPACT_LAYOUT.into()),
        ("status-format[1]".into(), formats[0].clone().unwrap()),
        ("status-format[3]".into(), String::new()),
        ("status-format[4]".into(), formats[3].clone().unwrap()),
    ];
    assert_eq!(
        outcomes,
        [
            (lines.clone(), true),
            (lines.clone(), false),
            (lines.clone(), false),
            (lines, false)
        ]
    );
}

#[test]
fn compact_hooks_are_gated_and_swap_hook_is_version_scoped() {
    let hooks = [false, true].map(|after_swap| {
        status_commands(
            "room",
            MuxStatusBar::Compact,
            "on",
            &std::array::from_fn(|_| None),
            after_swap,
        )
        .into_iter()
        .filter(|command| command[0] == "set-hook")
        .collect::<Vec<_>>()
    });
    assert_eq!(hooks.iter().map(Vec::len).collect::<Vec<_>>(), [7, 8]);
    let events: Vec<_> = hooks[0].iter().map(|command| command[2].as_str()).collect();
    assert_eq!(
        events,
        [
            "session-window-changed[241]",
            "window-linked[241]",
            "window-unlinked[241]",
            "window-renamed[241]",
            "alert-activity[241]",
            "alert-bell[241]",
            "alert-silence[241]"
        ]
    );
    assert_eq!(hooks[1].last().unwrap()[2], "after-swap-window[241]");
    for command in hooks.iter().flatten() {
        assert_eq!(command[1], "-g");
        assert!(
            command[3].starts_with("if-shell -F '#{==:#{@rimz_status},compact}'"),
            "{command:?}"
        );
        assert!(command[3].contains("window_end_flag"), "{command:?}");
        assert!(!command[3].contains("loop_last_flag"), "{command:?}");
        assert_eq!(command[3].matches("#{W:").count(), 1, "{command:?}");
        assert!(command[3].contains(":^"), "{command:?}");
    }
    assert!(COMPACT_LAYOUT.contains("#{@rimz_tab_strip}"));
    assert!(!COMPACT_LAYOUT.contains("#{W:"));
}

#[test]
fn swap_refresh_is_in_the_command_list_and_targets_the_session() {
    let commands = swap_window_commands("room", "rimzd", "0");
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0],
        ["swap-window", "-d", "-s", "room:rimzd", "-t", "room:0"]
    );
    assert_eq!(
        &commands[1][..5],
        [
            "if-shell",
            "-F",
            "-t",
            "room",
            "#{==:#{@rimz_status},compact}"
        ]
    );
    assert!(commands[1][5].contains("'-t' 'room:^' '@rimz_tab_start'"));
    assert!(commands[1][5].contains("'-t' 'room' '@rimz_tab_strip'"));
    assert!(commands[1][5].contains("window_end_flag"));
}
