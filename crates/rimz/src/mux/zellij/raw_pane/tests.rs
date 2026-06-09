use super::*;
use crate::ids::{MuxName, PaneId};
use crate::mux::SidebarWidth;
use crate::schema::pane_topology::{PaneTopologyCache, PaneTopologyPane};

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
    assert!(!parsed[0].is_floating);
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
          {"id": 4, "is_plugin": false, "is_suppressed": false, "exited": true, "tab_id": 0},
          {"id": 5, "is_plugin": false, "is_suppressed": false, "is_floating": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let live: Vec<u64> = parsed
        .iter()
        .filter(|p| p.is_live_terminal())
        .map(|p| p.id)
        .collect();
    // Only the plain live terminal pane survives; plugin, suppressed, held,
    // exited, and floating panes are all dropped.
    assert_eq!(live, vec![0]);
}
#[test]
fn floating_pane_teardown_targets_only_the_anchor_tab() {
    let json = r#"[
          {"id": 3, "is_plugin": true,  "is_suppressed": false, "tab_id": 9},
          {"id": 30, "is_plugin": false, "is_suppressed": false, "is_floating": true, "tab_id": 9},
          {"id": 3, "is_plugin": false, "is_suppressed": false, "tab_id": 1},
          {"id": 26, "is_plugin": false, "is_suppressed": false, "is_floating": true, "tab_id": 1},
          {"id": 27, "is_plugin": false, "is_suppressed": false, "is_floating": true, "tab_id": 2},
          {"id": 28, "is_plugin": true,  "is_suppressed": false, "is_floating": true, "tab_id": 1}
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_3");

    assert_eq!(
        floating_panes_in_anchor_view(&parsed, &anchor),
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_26")]
    );
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
                is_floating: false,
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
                is_floating: false,
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
