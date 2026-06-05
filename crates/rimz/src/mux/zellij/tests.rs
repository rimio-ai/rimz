use super::*;

#[test]
fn version_parser_accepts_three_dot_form() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);
}

#[test]
fn min_version_threshold_holds() {
    assert!((0, 41, 0) >= MIN_ZELLIJ_VERSION);
    assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
    assert!((0, 40, 9) < MIN_ZELLIJ_VERSION);
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = ZellijBackend::default();
    backend
        .version
        .set("zellij 9.9.9".to_owned())
        .expect("a fresh instance has not probed yet");
    // The cache is consulted before any probe: the seeded value comes back
    // verbatim — no `zellij --version` fork, no overwrite by a real binary.
    assert_eq!(backend.version().expect("cached version"), "zellij 9.9.9");
}

#[test]
fn mouse_click_through_args_gate_on_version() {
    // Older or unknown Zellij does not know the flag — omit it.
    assert!(mouse_click_through_args(true, None).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 43, 9))).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 41, 0))).is_empty());
    assert!(mouse_click_through_args(false, Some((0, 44, 3))).is_empty());
    // The release that added the option, and newer, carry it.
    let expected = vec!["--mouse-click-through".to_owned(), "true".to_owned()];
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 0))), expected);
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 3))), expected);
}

#[test]
fn zellij_options_render_room_defaults() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(
        !args.iter().any(|arg| arg == "--mouse-mode"),
        "`--mouse-mode true` disables mouse reporting on Zellij 0.44.3; \
             rely on Zellij's default enabled state"
    );
    assert!(has("--mouse-click-through", "true"));
    assert!(has("--focus-follows-mouse", "false"));
    assert!(has("--pane-frames", "false"));
    assert!(has("--copy-clipboard", "system"));
    assert!(has("--support-kitty-keyboard-protocol", "true"));
    assert!(has("--session-serialization", "false"));
}

#[test]
fn zellij_options_render_mouse_opt_out() {
    let config = ZellijConfig {
        mouse_mode: false,
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&config, Some((0, 44, 3)));
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--mouse-mode" && pair[1] == "false")
    );
}

#[test]
fn session_serialization_is_not_version_gated() {
    // Unlike `mouse-click-through`, the flag predates Rimz's Zellij floor, so
    // it must be present even when the version probe returns nothing.
    let args = zellij_options_args(&ZellijConfig::default(), None);
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--session-serialization", "false"));
    // And the gated option is correctly absent at an unknown version.
    assert!(!args.iter().any(|arg| arg == "--mouse-click-through"));
}

#[test]
fn raw_pane_deserializes_minimal_shape() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "is_focused": true, "tab_id": 0},
          {"id": 2, "is_plugin": true,  "is_suppressed": false, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert_eq!(parsed.len(), 2);
    assert!(!parsed[0].is_plugin);
    assert!(parsed[0].is_focused);
    assert!(parsed[1].is_plugin);
    assert!(!parsed[1].is_focused);
}

#[test]
fn raw_pane_command_uses_terminal_command_and_sidebar_title() {
    let json = r#"[
          {
            "id": 0,
            "is_plugin": false,
            "tab_id": 0,
            "title": "rimz-sidebar",
            "terminal_command": "/home/me/.cargo/bin/rimz sidebar serve --mux zellij"
          },
          {
            "id": 1,
            "is_plugin": false,
            "tab_id": 0,
            "title": "claude remote-control --spawn worktree",
            "terminal_command": "claude remote-control --spawn worktree"
          },
          {
            "id": 2,
            "is_plugin": false,
            "tab_id": 0,
            "title": "shell",
            "pane_command": "zsh",
            "terminal_command": "ignored"
          },
          {
            "id": 3,
            "is_plugin": false,
            "tab_id": 0,
            "title": "claude",
            "pane_command": "",
            "terminal_command": "claude remote-control --spawn worktree"
          }
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(
        parsed[0].pane_ref_command().as_deref(),
        Some("rimz-sidebar"),
        "a title-identified sidebar stays chrome even when command fields are missing or point at the launcher",
    );
    assert_eq!(
        parsed[1].pane_ref_command().as_deref(),
        Some("claude remote-control --spawn worktree"),
        "Zellij's full terminal command is the host-process signal",
    );
    assert_eq!(
        parsed[2].pane_ref_command().as_deref(),
        Some("zsh"),
        "pane_command remains the foreground-command source when present",
    );
    assert_eq!(
        parsed[3].pane_ref_command().as_deref(),
        Some("claude remote-control --spawn worktree"),
        "a present-but-empty field falls through the ladder instead of masking a later one",
    );
}

#[test]
fn views_with_sidebars_groups_by_tab_and_normalizes_pane_ids() {
    // tab 0: a working pane plus two sidebar panes (a duplicate); tab 1: a
    // sidebar-only tab; the plugin pane never counts as working.
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 3, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 9, "is_plugin": true,  "tab_id": 0, "title": "zellij:status"},
          {"id": 4, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let views = views_with_sidebars(&panes);
    assert_eq!(views.len(), 2);

    assert_eq!(views[0].view, "0");
    assert!(views[0].has_working);
    assert!(!views[0].has_daemon_host);
    assert_eq!(
        views[0].sidebar_panes,
        vec![
            PaneId::from_parts(MuxName::Zellij, "terminal_2"),
            PaneId::from_parts(MuxName::Zellij, "terminal_3"),
        ],
    );

    // tab 1 is a sidebar-only orphan: no working pane and no daemon host.
    assert_eq!(views[1].view, "1");
    assert!(!views[1].has_working);
    assert!(!views[1].has_daemon_host);
    assert_eq!(views[1].sidebar_panes.len(), 1);
}

#[test]
fn views_with_sidebars_ignores_daemon_hosts_as_working_panes() {
    let json = r#"[
          {
            "id": 2,
            "is_plugin": false,
            "tab_id": 0,
            "title": "/home/marvin/.cargo/bin/rimz codex app-server serve --workspace-id ws_1 --session-name rimz-home",
            "pane_command": "/home/marvin/.cargo/bin/rimz codex app-server serve --workspace-id ws_1 --session-name rimz-home"
          },
          {
            "id": 3,
            "is_plugin": false,
            "tab_id": 1,
            "title": "claude remote-control --spawn worktree",
            "terminal_command": "claude remote-control --spawn worktree"
          }
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let views = views_with_sidebars(&panes);

    assert_eq!(views.len(), 2);
    assert_eq!(views[0].view, "0");
    assert!(!views[0].has_working);
    assert!(
        views[0].has_daemon_host,
        "a daemon host marks the view so reload never collapses it as an orphan",
    );
    assert!(views[0].sidebar_panes.is_empty());
    assert!(
        views[1].has_daemon_host && !views[1].has_working,
        "a host reported only via terminal_command is still a daemon host, not user work",
    );
}

#[test]
fn live_terminal_excludes_plugin_suppressed_and_dead_panes() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "tab_id": 0},
          {"id": 1, "is_plugin": true,  "is_suppressed": false, "tab_id": 0},
          {"id": 2, "is_plugin": false, "is_suppressed": true,  "tab_id": 0},
          {"id": 3, "is_plugin": false, "is_suppressed": false, "is_held": true, "tab_id": 0},
          {"id": 4, "is_plugin": false, "is_suppressed": false, "exited": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let live: Vec<u64> = parsed
        .iter()
        .filter(|p| p.is_live_terminal())
        .map(|p| p.id)
        .collect();
    // Only the plain live terminal pane survives; plugin, suppressed, held,
    // and exited panes are all dropped.
    assert_eq!(live, vec![0]);
}

#[test]
fn held_sidebar_is_not_healthy() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": true, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "bash", "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert!(!has_healthy_sidebar(&parsed));
}

#[test]
fn running_sidebar_is_healthy() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {"id": 1, "is_plugin": true, "title": "compact-bar", "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert!(has_healthy_sidebar(&parsed));
}

#[test]
fn held_command_pane_is_the_resurrection_fingerprint() {
    // A resurrected room: the sidebar runs, but a command pane is held at a
    // "Waiting to run" prompt. `has_healthy_sidebar` alone would miss it, so
    // `classify_session_panes` also checks for a suspended command pane.
    let resurrected = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(resurrected).unwrap();
    assert!(has_healthy_sidebar(&parsed));
    assert!(has_suspended_command_pane(&parsed));

    // A clean room: sidebar and command pane both running.
    let clean = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": false, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(clean).unwrap();
    assert!(!has_suspended_command_pane(&parsed));

    // A held *sidebar* is the sidebar signal, not a command-pane signal.
    let held_sidebar = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(held_sidebar).unwrap();
    assert!(!has_suspended_command_pane(&parsed));
}

#[test]
fn session_panes_classify_clean_sidebar_and_suspended_commands() {
    let clean = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": false, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(clean).unwrap();
    assert_eq!(classify_session_panes(&parsed), SessionCleanliness::Clean);

    let held_sidebar = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": true, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": false, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(held_sidebar).unwrap();
    assert_eq!(
        classify_session_panes(&parsed),
        SessionCleanliness::MissingSidebar,
    );

    let no_sidebar = r#"[
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": false, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(no_sidebar).unwrap();
    assert_eq!(
        classify_session_panes(&parsed),
        SessionCleanliness::MissingSidebar,
    );

    let suspended_command = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {"id": 1, "is_plugin": false, "title": "claude", "is_held": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(suspended_command).unwrap();
    assert_eq!(
        classify_session_panes(&parsed),
        SessionCleanliness::SuspendedCommandPane,
    );
}

#[test]
fn missing_sidebar_is_not_healthy() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "title": "bash", "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert!(!has_healthy_sidebar(&parsed));
}

#[test]
fn transient_empty_detects_blank_list_panes_output() {
    assert!(is_transient_empty(b""));
    assert!(is_transient_empty(b"  \n\t"));
    // A real, parseable answer — even an empty pane set — is not transient.
    assert!(!is_transient_empty(b"[]"));
    assert!(!is_transient_empty(b"[{\"id\":0}]"));
}

#[test]
fn ansi_strip_drops_color_codes() {
    let stripped = strip_ansi("\x1b[32mfoo\x1b[0m bar");
    assert_eq!(stripped, "foo bar");
}

#[test]
fn capture_trim_keeps_last_requested_lines() {
    let (raw, lines) = trim_capture("a\nb\nc\nd\n".to_owned(), Some(2));
    assert_eq!(lines, vec!["c", "d"]);
    assert_eq!(raw, "c\nd\n");
}

#[test]
fn session_state_classifies_list_sessions_lines() {
    assert_eq!(
        session_state_from_line("rimz-query-engine [Created 6m ago]", "rimz-query-engine"),
        Some(SessionState::Live),
    );
    assert_eq!(
        session_state_from_line(
            "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
            "rimz-query-engine",
        ),
        Some(SessionState::Exited),
    );
    // A colorized line (no `--no-formatting`) still parses via `strip_ansi`.
    assert_eq!(
        session_state_from_line(
            "\x1b[32;1mrimz-query-engine\x1b[m [Created ago] (\x1b[31;1mEXITED\x1b[m - resurrect)",
            "rimz-query-engine",
        ),
        Some(SessionState::Exited),
    );
    // A different session's line is not a match.
    assert_eq!(
        session_state_from_line("other [Created 6m ago]", "rimz-query-engine"),
        None,
    );
}

#[test]
fn live_session_name_excludes_exited_rows() {
    assert_eq!(
        live_session_name_from_line("rimz-query-engine [Created 6m ago]"),
        Some("rimz-query-engine".to_owned()),
    );
    assert_eq!(
        live_session_name_from_line(
            "rimz-query-engine [Created 6m ago] (EXITED - attach to resurrect)",
        ),
        None,
    );
}
