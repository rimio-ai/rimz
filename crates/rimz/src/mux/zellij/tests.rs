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
    assert!(has("--default-mode", "locked"));
    assert!(has("--mouse-click-through", "true"));
    assert!(has("--advanced-mouse-actions", "false"));
    assert!(has("--mouse-hover-effects", "false"));
    assert!(has("--focus-follows-mouse", "false"));
    assert!(has("--pane-frames", "false"));
    assert!(has("--copy-clipboard", "system"));
    assert!(has("--support-kitty-keyboard-protocol", "true"));
    assert!(has("--session-serialization", "false"));
}

#[test]
fn zellij_options_gate_newer_mouse_flags() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 42, 9)));
    assert!(
        !args.iter().any(|arg| arg == "--advanced-mouse-actions"),
        "Zellij before 0.43 rejects advanced mouse action options"
    );
    assert!(
        !args.iter().any(|arg| arg == "--mouse-hover-effects"),
        "Zellij before 0.44 rejects mouse hover effect options"
    );

    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 43, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "false"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));
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
    assert!(!args.iter().any(|arg| arg == "--advanced-mouse-actions"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));
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
fn raw_pane_view_position_prefers_list_panes_tab_position() {
    let json = r#"[
          {"id": 8, "is_plugin": false, "tab_id": 42, "tab_position": 1}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(parsed[0].tab_id, 42);
    assert_eq!(parsed[0].view_position(), 1);
}

#[test]
fn topology_cache_accepts_legacy_tab_id_as_position() {
    let json = r#"{
          "session_name": "rimz-test",
          "produced_at_ms": 1,
          "panes": [
            {"id": 8, "is_plugin": false, "tab_id": 3}
          ]
        }"#;
    let cache: PaneTopologyCache = serde_json::from_str(json).unwrap();

    assert_eq!(cache.panes[0].tab_position, 3);
}

#[test]
fn topology_cache_freshness_honors_requested_floor() {
    let cache = PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: 100,
        panes: Vec::new(),
    };

    assert!(crate::sidebar::cache::pane_topology_cache_is_fresh(
        &cache,
        101,
        Some(100),
    ));
    assert!(!crate::sidebar::cache::pane_topology_cache_is_fresh(
        &cache,
        101,
        Some(101),
    ));
}

#[test]
fn parse_focused_client_panes_reads_unique_terminal_ids() {
    let output = b"CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                   1         terminal_30    codex\n\
                   2         terminal_30    codex\n\
                   3         terminal_4     claude\n";
    let panes = parse_focused_client_panes(output);
    assert_eq!(
        panes,
        vec![
            PaneId::from_parts(MuxName::Zellij, "terminal_30"),
            PaneId::from_parts(MuxName::Zellij, "terminal_4"),
        ]
    );
}

#[test]
fn parse_focused_client_panes_ignores_headers_and_plugins() {
    let output = b"\x1b[32;1mCLIENT_ID\x1b[m ZELLIJ_PANE_ID RUNNING_COMMAND\n\
                   1 plugin_2 rimz-presence-zellij\n\
                   2 - unknown\n";
    assert!(parse_focused_client_panes(output).is_empty());
}

#[test]
fn raw_pane_splits_foreground_spawn_and_sidebar_title() {
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
        parsed[0].display_command().as_deref(),
        Some("rimz-sidebar"),
        "a title-identified sidebar stays chrome even when command fields are missing or point at the launcher",
    );
    assert_eq!(
        parsed[1].display_command().as_deref(),
        None,
        "the spawn command no longer masquerades as foreground display",
    );
    assert_eq!(
        parsed[1].spawn_command(),
        Some("claude remote-control --spawn worktree"),
        "Zellij's full terminal command remains the host-process identity signal",
    );
    assert_eq!(
        parsed[2].display_command().as_deref(),
        Some("zsh"),
        "pane_command remains the foreground-command source when present",
    );
    assert_eq!(parsed[2].spawn_command(), Some("ignored"));
    assert_eq!(
        parsed[3].display_command().as_deref(),
        None,
        "an empty foreground field does not fall through to spawn display",
    );
    assert_eq!(
        parsed[3].spawn_command(),
        Some("claude remote-control --spawn worktree")
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
fn topology_cache_panes_feed_the_existing_classifier() {
    let cache = PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: 1,
        panes: vec![
            PaneTopologyPane {
                id: 6,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_focused: false,
                tab_position: 0,
                tab_name: Some("main".to_owned()),
                pane_columns: Some(20),
                pane_x: Some(0),
                title: Some("rimz-sidebar".to_owned()),
                pane_command: Some("rimz-sidebar".to_owned()),
                terminal_command: Some("rimz sidebar serve".to_owned()),
            },
            PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: true,
                exited: false,
                is_suppressed: false,
                is_focused: true,
                tab_position: 0,
                tab_name: Some("main".to_owned()),
                pane_columns: Some(100),
                pane_x: Some(20),
                title: Some("claude".to_owned()),
                pane_command: Some("claude".to_owned()),
                terminal_command: Some("rimz agents exec claude".to_owned()),
            },
        ],
    };
    let panes = raw_panes_from_topology(cache);

    assert_eq!(
        classify_session_panes(&panes),
        SessionCleanliness::SuspendedCommandPane,
    );
    assert_eq!(panes[0].display_command().as_deref(), Some("rimz-sidebar"));
    assert_eq!(panes[1].foreground_command(), Some("claude"));
    assert_eq!(panes[1].spawn_command(), Some("rimz agents exec claude"));
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

#[test]
fn new_tab_template_sidebar_cols_reads_the_fixed_width() {
    let layout = r#"
layout {
    tab {
        pane split_direction="vertical" {
            pane name="rimz-sidebar" size="24%"
            pane
        }
    }
    new_tab_template {
        pane split_direction="vertical" {
            pane name="rimz-sidebar" size=72
            pane focus=true
        }
    }
}
"#;
    assert_eq!(new_tab_template_sidebar_cols(layout), NonZeroU16::new(72));
}

#[test]
fn new_tab_template_sidebar_cols_ignores_percentage_sizes() {
    let layout = r#"
layout {
    new_tab_template {
        pane split_direction="vertical" {
            pane name="rimz-sidebar" size="24%"
            pane focus=true
        }
    }
}
"#;
    assert_eq!(new_tab_template_sidebar_cols(layout), None);
}

#[test]
fn raw_pane_deserializes_tab_name_and_geometry() {
    // The identity and geometry fields Zellij 0.44 actually emits per terminal
    // pane — no live command, cwd, or pid fields exist in its `list-panes -j`
    // output.
    let json = r#"[{
          "id": 1, "is_plugin": false, "tab_id": 0, "tab_name": "rimzd",
          "pane_x": 60, "pane_columns": 118,
          "title": "claude remote-control --spawn worktree",
          "terminal_command": "claude remote-control --spawn worktree"
        }]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert_eq!(parsed[0].tab_name.as_deref(), Some("rimzd"));
    assert_eq!(parsed[0].pane_x, Some(60));
    assert_eq!(parsed[0].pane_columns, Some(118));
    assert_eq!(
        parsed[0].terminal_command.as_deref(),
        Some("claude remote-control --spawn worktree"),
    );
}

#[test]
fn rimzd_tab_name_marks_the_daemon_view_without_command_fields() {
    // Zellij 0.44 reports no command fields, and the Claude host re-execs
    // into a bare versioned binary anyway — the tab name alone must carry
    // the daemon classification so reload never treats `rimzd` as work.
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "tab_name": "rimzd",
           "title": "claude"},
          {"id": 5, "is_plugin": false, "tab_id": 1, "tab_name": "Tab #2",
           "title": "zsh"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let views = views_with_sidebars(&panes);
    assert_eq!(views.len(), 2);
    assert!(views[0].has_daemon_host, "rimzd tab is the daemon view");
    assert!(!views[0].has_working);
    assert!(views[1].has_working, "an ordinary tab still reads as work");
    assert!(!views[1].has_daemon_host);
}

#[test]
fn tab_extent_cols_takes_extents_not_the_sum() {
    // A left sidebar beside two vertically stacked panes: the sum (60 +
    // 238 + 238 = 536) would nearly double the real tab width (298).
    let json = r#"[
          {"id": 0, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh",
           "pane_x": 60, "pane_columns": 238},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "vim",
           "pane_x": 60, "pane_columns": 238},
          {"id": 3, "is_plugin": false, "tab_id": 1, "title": "zsh",
           "pane_x": 0, "pane_columns": 120}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert_eq!(tab_extent_cols(&panes, 0), 298);
    assert_eq!(tab_extent_cols(&panes, 1), 120);
    assert_eq!(tab_extent_cols(&panes, 9), 0, "an absent tab has no width");
}

#[test]
fn sidebar_geometry_off_spec_trips_on_the_mis_mounted_shape_only() {
    // Tab 0: the mis-mounted shape — sidebar on the right at 50%.
    // Tab 1: a healthy layout-born sidebar — left at ~21%.
    // Tab 2: docked left but still half the tab (dock landed, resize lost).
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh",
           "pane_x": 0, "pane_columns": 149},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar",
           "pane_x": 149, "pane_columns": 149},
          {"id": 3, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 64},
          {"id": 4, "is_plugin": false, "tab_id": 1, "title": "zsh",
           "pane_x": 64, "pane_columns": 234},
          {"id": 5, "is_plugin": false, "tab_id": 2, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 149},
          {"id": 6, "is_plugin": false, "tab_id": 2, "title": "zsh",
           "pane_x": 149, "pane_columns": 149}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let width = SidebarWidth::default();
    let by_id = |id: u64| panes.iter().find(|pane| pane.id == id).unwrap();
    assert!(
        sidebar_geometry_off_spec(by_id(2), &panes, width),
        "right-docked 50% sidebar is off-spec",
    );
    assert!(
        !sidebar_geometry_off_spec(by_id(3), &panes, width),
        "a healthy ~21% layout-born sidebar is never churned",
    );
    assert!(
        sidebar_geometry_off_spec(by_id(5), &panes, width),
        "left but 50%-wide still wants the resize",
    );
}

#[test]
fn sidebar_width_at_the_cap_is_never_off_spec() {
    // A pane born fixed at `max_cols` can exceed 45% of a narrow client's tab;
    // the cap itself is the width verdict and never needs repair.
    let width = SidebarWidth::default();
    let cap = width.cap_cols();
    assert!(
        !sidebar_width_off_spec(cap, 140, width),
        "cap-wide on a 140-col tab is a width verdict, not a mis-mount",
    );
    assert!(
        sidebar_width_off_spec(149, 298, width),
        "the 50% mis-mount is past both the trigger and the cap",
    );
    assert!(
        sidebar_width_off_spec(60, 120, width),
        "an under-cap 50% mis-mount still wants the layout width",
    );
    assert!(
        sidebar_width_off_spec(90, 298, width),
        "30% on a wide tab is still off-spec when it exceeds max_cols",
    );
}

#[test]
fn sidebar_geometry_without_coordinates_is_never_off_spec() {
    // Builds that omit geometry give convergence nothing to act on.
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "zsh"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert!(!sidebar_geometry_off_spec(
        &panes[0],
        &panes,
        SidebarWidth::default()
    ));
}

#[test]
fn new_pane_stdout_parses_only_a_bare_terminal_id() {
    assert_eq!(
        parse_new_pane_id(" terminal_58\n"),
        Some("terminal_58".to_owned()),
    );
    // Cross-talked responses from concurrent action clients: an empty
    // body, another command's JSON, a plugin id, or trailing garbage are
    // all hints we must refuse — never errors, never pane ids.
    assert_eq!(parse_new_pane_id(""), None);
    assert_eq!(
        parse_new_pane_id("[{\"id\": 3, \"is_plugin\": false}]"),
        None
    );
    assert_eq!(parse_new_pane_id("plugin_3"), None);
    assert_eq!(parse_new_pane_id("terminal_"), None);
    assert_eq!(parse_new_pane_id("terminal_5x"), None);
    assert_eq!(parse_new_pane_id("terminal_5 terminal_6"), None);
}

#[test]
fn mounted_sidebar_discovery_prefers_the_hint_then_the_new_pane() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 7, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 9, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let before: std::collections::HashSet<u64> = [1].into();
    // The hint wins when it names a mounted sidebar pane in the tab.
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, Some(7)), Some(7));
    // Without a usable hint, the new (not-in-before) sidebar pane is it.
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, None), Some(7));
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, Some(42)), Some(7));
    // Another tab's sidebar never matches; a tab with none reports none.
    assert_eq!(mounted_sidebar_pane(&panes, 2, &before, None), None);
}

#[test]
fn mounted_sidebar_discovery_ignores_preexisting_and_non_sidebar_panes() {
    let json = r#"[
          {"id": 3, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 4, "is_plugin": false, "tab_id": 0, "title": "vim"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    // The tab's only sidebar pane predates the add: the mount never landed.
    let before: std::collections::HashSet<u64> = [3, 4].into();
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, None), None);
}

#[test]
fn presence_plugin_floor_admits_the_tile_line_only() {
    // The floor is the `zellij-tile` pin: 0.44.x loads, anything older keeps
    // the pane poll (and stays above MIN_ZELLIJ_VERSION for everything else).
    assert!((0, 44, 0) >= PRESENCE_PLUGIN_MIN_ZELLIJ);
    assert!((0, 44, 3) >= PRESENCE_PLUGIN_MIN_ZELLIJ);
    assert!((0, 43, 9) < PRESENCE_PLUGIN_MIN_ZELLIJ);
    assert!(PRESENCE_PLUGIN_MIN_ZELLIJ >= MIN_ZELLIJ_VERSION);
}

#[test]
fn presence_plugin_configuration_pins_workspace_and_rimz() {
    let configuration =
        presence_plugin_configuration(&presence_opts("rimz-test", "/home/user/.cargo/bin/rimz"));
    assert_eq!(
        configuration,
        "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test,rimz_bin=/home/user/.cargo/bin/rimz",
    );
}

#[test]
fn presence_plugin_configuration_omits_an_inexpressible_rimz_path() {
    // Zellij parses the configuration by splitting on `,` then `=`; a path
    // containing either separator would mis-parse into a broken poke argv, so
    // it is omitted and the plugin falls back to `rimz` on the host PATH.
    for weird in ["/tmp/a,b/rimz", "/tmp/a=b/rimz"] {
        let configuration = presence_plugin_configuration(&presence_opts("rimz-test", weird));
        assert_eq!(
            configuration, "workspace_id=ws_0123456789abcdef01234567,session_name=rimz-test",
            "{weird} must be omitted, not shipped mis-parsable",
        );
    }
}

#[test]
fn presence_plugin_configuration_omits_an_inexpressible_session_name() {
    for weird in ["rimz,test", "rimz=test"] {
        let configuration =
            presence_plugin_configuration(&presence_opts(weird, "/home/user/.cargo/bin/rimz"));
        assert_eq!(
            configuration,
            "workspace_id=ws_0123456789abcdef01234567,rimz_bin=/home/user/.cargo/bin/rimz",
            "{weird} must be omitted, not shipped mis-parsable",
        );
    }
}

fn presence_opts(session_name: &str, rimz_bin: &str) -> crate::mux::PresencePluginOptions {
    crate::mux::PresencePluginOptions {
        session_name: session_name.to_owned(),
        workspace_id: crate::ids::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
        wasm: std::path::PathBuf::from("/tmp/rimz-presence-zellij.wasm"),
        rimz_bin: std::path::PathBuf::from(rimz_bin),
        converge: false,
    }
}

#[test]
fn materialize_presence_plugin_bytes_writes_stable_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
        .unwrap()
        .unwrap();
    assert!(path.ends_with("rimz/plugins/rimz-presence-zellij.wasm"));
    assert_eq!(std::fs::read(&path).unwrap(), b"wasm-bytes");

    let same_path = materialize_presence_plugin_bytes(b"wasm-bytes", dir.path())
        .unwrap()
        .unwrap();
    assert_eq!(same_path, path);

    materialize_presence_plugin_bytes(b"new-bytes", dir.path()).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"new-bytes");
}

#[test]
fn empty_presence_plugin_embed_materializes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        materialize_presence_plugin_bytes(b"", dir.path())
            .unwrap()
            .is_none()
    );
}
