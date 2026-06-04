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

#[test]
fn sidebar_layout_carries_a_bottom_bar() {
    use crate::ids::WorkspaceId;
    let opts = SidebarPaneOptions {
        session_name: "rimz-bar".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bar")),
        cwd: PathBuf::from("/tmp/rimz-bar"),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(None),
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };
    let layout = render_sidebar_layout(&opts).expect("render layout");
    assert!(
        layout.contains("compact-bar"),
        "the sidebar layout overrides Zellij's default tab template, so it must \
             re-add a bottom bar plugin or the tab/status bar vanishes:\n{layout}",
    );
}

#[test]
fn sidebar_layout_focuses_an_explicit_terminal_in_every_tab() {
    use crate::ids::WorkspaceId;
    let opts = SidebarPaneOptions {
        session_name: "rimz-focus".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-focus")),
        cwd: PathBuf::from("/tmp/rimz-focus"),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(None),
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };
    let layout = render_sidebar_layout(&opts).expect("render layout");
    // The template must spell out the focused terminal instead of relying
    // on a nested `children` placeholder: every template-born tab needs a
    // right pane with focus, never a bare or focused sidebar.
    assert!(
        layout.contains("pane focus=true"),
        "the layout must focus an explicit terminal pane:\n{layout}",
    );
    assert!(
        !layout.contains("children"),
        "the layout must not depend on `children`: placeholder semantics \
             can misplace focus or omit the right terminal in template-born tabs:\n{layout}",
    );
    // The bare `tab` node is load-bearing: with a `new_tab_template`
    // present and no tab node, Zellij 0.44.3 kills the background session
    // instead of creating the implicit first tab.
    assert!(
        layout.contains("tab focus=true"),
        "the layout must carry an explicit birth tab alongside the \
             templates or the detached session dies:\n{layout}",
    );
}

#[test]
fn sidebar_layout_pins_fixed_cols_attached_and_percent_detached() {
    use crate::ids::WorkspaceId;
    let opts = SidebarPaneOptions {
        session_name: "rimz-width".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-width")),
        cwd: PathBuf::from("/tmp/rimz-width"),
        width: SidebarWidth::default(),
        // 30% of 120 is 36 ≤ the 72 cap — the under-cap verdict.
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };
    let layout = render_sidebar_layout(&opts).expect("render layout");
    // The birth tab spells the verdict's percentage share — a fixed size
    // wider than the detached session's default geometry kills the
    // session — and lands on the verdict when the launching client
    // attaches.
    assert!(
        layout.contains(r#"pane size="30%" name="rimz-sidebar""#),
        "the default_tab_template births detached, so the verdict is its \
             percentage share:\n{layout}",
    );
    // Tabs the user opens later instantiate at live geometry, so the
    // new_tab_template pins the verdict exactly, as a bare KDL integer —
    // even under the cap. A raw percentage here re-evaluates against
    // whatever geometry instantiates the tab, which is exactly how the
    // cap used to vanish from a session.
    assert!(
        layout.contains(r#"pane size=36 name="rimz-sidebar""#),
        "the new_tab_template instantiates attached, so it pins the fixed \
             verdict:\n{layout}",
    );
    // Past the cap the same split holds: ⌊72·100/340⌋ = 21% detached,
    // the fixed cap attached.
    let capped = SidebarPaneOptions {
        birth_size: SidebarWidth::default().birth_size(Some(340)),
        ..opts
    };
    let layout = render_sidebar_layout(&capped).expect("render layout");
    assert!(
        layout.contains(r#"pane size="21%" name="rimz-sidebar""#),
        "the default_tab_template births detached, so a capped width is \
             its derived percentage:\n{layout}",
    );
    assert!(
        layout.contains(r#"pane size=72 name="rimz-sidebar""#),
        "the new_tab_template instantiates attached, so a capped width is \
             the fixed `max_cols` cap:\n{layout}",
    );
    let new_tab_template = layout
        .split("new_tab_template")
        .nth(1)
        .expect("layout carries a new_tab_template");
    assert!(
        !new_tab_template.contains('%'),
        "the new_tab_template carries no percentage:\n{layout}",
    );
}

fn host(argv: &[&str], cwd: &str) -> HostPane {
    HostPane {
        argv: argv.iter().map(|arg| arg.to_string()).collect(),
        cwd: PathBuf::from(cwd),
    }
}

fn background_view_opts(hosts: Vec<HostPane>) -> BackgroundViewOptions {
    use crate::ids::WorkspaceId;
    BackgroundViewOptions {
        name: "rimzd".to_owned(),
        hosts,
        sidebar: SidebarPaneOptions {
            session_name: "rimz-bg".to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/proj/root")),
            cwd: PathBuf::from("/proj/worktree"),
            width: SidebarWidth::default(),
            birth_size: SidebarWidth::default().birth_size(None),
            rimz_bin: PathBuf::from("/usr/bin/rimz"),
            replace_existing: false,
            config: crate::config::MultiplexerConfig::default(),
            resume_panes: Vec::new(),
        },
    }
}

#[test]
fn background_view_layout_runs_the_host_beside_the_sidebar() {
    let layout = render_background_view_layout(&background_view_opts(vec![host(
        &["claude", "remote-control", "--spawn", "worktree"],
        "/proj/root",
    )]))
    .expect("render background view layout");
    // The host is the focused right pane, born unsuspended, and closes with
    // its process — an exit means the host is gone.
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(
        layout.contains(r#"args "remote-control" "--spawn" "worktree""#),
        "{layout}",
    );
    assert!(layout.contains("pane focus=true"), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    assert!(layout.contains("close_on_exit true"), "{layout}");
    // The global sidebar is docked on the left, running the renderer.
    assert!(layout.contains(r#"name="rimz-sidebar""#), "{layout}");
    assert!(layout.contains(r#""sidebar" "serve""#), "{layout}");
    // A bottom bar, mirroring the working-tab template.
    assert!(layout.contains("compact-bar"), "{layout}");
    // Each pane carries its own cwd: the sidebar from the worktree, the host
    // from the project root.
    assert!(layout.contains(r#"cwd="/proj/worktree""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");
}

#[test]
fn background_view_layout_stacks_two_hosts_focusing_the_first() {
    let layout = render_background_view_layout(&background_view_opts(vec![
        host(&["claude", "remote-control"], "/proj/root"),
        host(
            &["/usr/bin/rimz", "codex", "app-server", "serve"],
            "/proj/worktree",
        ),
    ]))
    .expect("render background view layout");
    // Both hosts are present beside the sidebar.
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(layout.contains(r#"command "/usr/bin/rimz""#), "{layout}");
    assert!(
        layout.contains(r#"args "codex" "app-server" "serve""#),
        "{layout}",
    );
    // Exactly one pane takes focus — the first host (the interactive Claude
    // host), never the broker.
    assert_eq!(layout.matches("focus=true").count(), 1, "{layout}");
}

#[test]
fn background_view_layout_rejects_no_hosts() {
    assert!(render_background_view_layout(&background_view_opts(vec![])).is_err());
}

fn daemon_view(hosts: Vec<HostPane>) -> DaemonView {
    DaemonView {
        name: "rimzd".to_owned(),
        hosts,
    }
}

fn resume_pane(label: &str, argv: &[&str], cwd: &str) -> ResumePane {
    ResumePane {
        command: argv.iter().map(|arg| arg.to_string()).collect(),
        cwd: PathBuf::from(cwd),
        label: label.to_owned(),
    }
}

#[test]
fn session_layout_seeds_resumed_agents_focusing_the_first() {
    let opts = background_view_opts(vec![]).sidebar;
    let resume = vec![
        resume_pane(
            "claude:feature",
            &["claude", "--resume", "sess-1"],
            "/proj/feature",
        ),
        resume_pane("codex:main", &["codex", "resume", "sess-2"], "/proj/main"),
    ];
    let layout = render_session_layout(&opts, None, &resume).expect("render resume layout");
    // Each agent runs its resume CLI in its own worktree, born unsuspended.
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(layout.contains(r#"args "--resume" "sess-1""#), "{layout}");
    assert!(layout.contains(r#"command "codex""#), "{layout}");
    assert!(layout.contains(r#"args "resume" "sess-2""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/feature""#), "{layout}");
    assert!(layout.contains(r#"cwd="/proj/main""#), "{layout}");
    assert!(layout.contains("start_suspended false"), "{layout}");
    // One tab per agent, named by label; the first (most-recent) takes focus.
    assert!(
        layout.contains(r#"tab name="claude:feature" focus=true"#),
        "the freshest resumed agent leads:\n{layout}",
    );
    assert!(
        !layout.contains(r#"tab name="codex:main" focus=true"#),
        "only the first resumed tab is focused:\n{layout}",
    );
    // A free working terminal tab still exists, unfocused (an agent has focus).
    assert!(
        layout.contains("    tab {"),
        "a bare working terminal tab remains:\n{layout}",
    );
    // Future user tabs inherit the sidebar+terminal template, no `children`.
    assert!(layout.contains("new_tab_template"), "{layout}");
    assert!(!layout.contains("children"), "{layout}");
}

#[test]
fn session_layout_without_daemon_or_resume_focuses_the_working_tab() {
    let opts = background_view_opts(vec![]).sidebar;
    let layout = render_session_layout(&opts, None, &[]).expect("render layout");
    // No agents, no daemon: the working terminal tab takes focus and there
    // are no named daemon/agent tabs to seed.
    assert!(layout.contains("tab focus=true"), "{layout}");
    assert!(
        !layout.contains("tab name="),
        "no daemon or agent tabs without a daemon or resume set:\n{layout}",
    );
}

#[test]
fn session_layout_with_daemon_leads_with_the_daemon_tab() {
    let bg = background_view_opts(vec![
        host(&["claude", "remote-control"], "/proj/root"),
        host(
            &["/usr/bin/rimz", "codex", "app-server", "serve"],
            "/proj/worktree",
        ),
    ]);
    let layout = render_session_layout(&bg.sidebar, Some(&daemon_view(bg.hosts.clone())), &[])
        .expect("render session layout with daemon");
    // The daemon tab is declared first — before the focused working tab — so
    // it leads. Zellij fixes tab order at birth (it can't reorder later).
    let daemon_at = layout.find(r#"tab name="rimzd""#).expect("daemon tab");
    let work_at = layout.find("tab focus=true").expect("working tab");
    assert!(
        daemon_at < work_at,
        "daemon tab must precede the working tab\n{layout}",
    );
    // Future user tabs inherit a sidebar + focused terminal via the
    // `new_tab_template`, which (unlike `default_tab_template` with explicit
    // tabs) needs no `children` and so dodges the focus-strand bug.
    assert!(layout.contains("new_tab_template"), "{layout}");
    assert!(!layout.contains("children"), "{layout}");
    // Both hosts and the sidebar are present beside each other.
    assert!(layout.contains(r#"command "claude""#), "{layout}");
    assert!(
        layout.contains(r#"args "codex" "app-server" "serve""#),
        "{layout}",
    );
    assert!(layout.contains(r#"name="rimz-sidebar""#), "{layout}");
    assert!(layout.contains("compact-bar"), "{layout}");
    // The host that leads the daemon view runs from the project root; the
    // sidebars inherit the session `--default-cwd`, so they carry no cwd.
    assert!(layout.contains(r#"cwd="/proj/root""#), "{layout}");
}

#[test]
fn session_layout_with_daemon_rejects_no_hosts() {
    assert!(
        render_session_layout(
            &background_view_opts(vec![]).sidebar,
            Some(&daemon_view(vec![])),
            &[],
        )
        .is_err()
    );
}

#[test]
fn sidebar_layout_starts_the_sidebar_without_a_run_prompt() {
    use crate::ids::WorkspaceId;
    let opts = SidebarPaneOptions {
        session_name: "rimz-run".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-run")),
        cwd: PathBuf::from("/tmp/rimz-run"),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(None),
        rimz_bin: PathBuf::from("/usr/bin/rimz"),
        replace_existing: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_panes: Vec::new(),
    };
    let layout = render_sidebar_layout(&opts).expect("render layout");
    assert!(
        layout.contains("start_suspended false"),
        "Zellij command panes default to a run prompt unless the layout \
             starts them explicitly:\n{layout}",
    );
    assert!(
        !layout.contains("start_suspended true"),
        "the sidebar pane must never be born suspended:\n{layout}",
    );
}
