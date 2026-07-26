use super::*;
use std::collections::HashSet;

use crate::ids::{MuxName, PaneId};
use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};

#[test]
fn raw_pane_position_metadata_accepts_live_and_topology_shapes() {
    let json = r#"[
          {
            "id": 8, "is_plugin": false, "tab_position": 1,
            "tab_name": "rimzd", "pane_x": 60, "pane_columns": 118,
            "title": "claude remote-control --spawn worktree",
            "terminal_command": "claude remote-control --spawn worktree"
          }
        ]"#;
    let parsed: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    assert_eq!(parsed[0].tab_position, 1);
    assert_eq!(parsed[0].view_position(), 1);
    assert_eq!(parsed[0].tab_name.as_deref(), Some("rimzd"));
    assert_eq!(parsed[0].pane_x, Some(60));
    assert_eq!(parsed[0].pane_columns, Some(118));
    assert_eq!(
        parsed[0].terminal_command.as_deref(),
        Some("claude remote-control --spawn worktree"),
    );

    let json = r#"{
          "session_name": "rimz-test",
          "produced_at_ms": 1,
          "panes": [
            {"id": 8, "is_plugin": false, "tab_id": 3}
          ]
        }"#;
    let cache: PaneTopologyCache = serde_json::from_str(json).unwrap();
    assert_eq!(cache.panes[0].tab_position, 3);
    let panes = cache.panes;
    assert_eq!(panes[0].tab_position, 3);
    assert_eq!(panes[0].view_position(), 3);
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
    let parsed: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();

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
fn listed_pane_includes_live_floating_but_live_terminal_does_not() {
    let json = r#"[
          {"id": 0, "is_plugin": false, "is_suppressed": false, "tab_id": 0},
          {"id": 1, "is_plugin": true,  "is_suppressed": false, "tab_id": 0},
          {"id": 2, "is_plugin": false, "is_suppressed": true,  "tab_id": 0},
          {"id": 3, "is_plugin": false, "is_suppressed": false, "is_held": true, "tab_id": 0},
          {"id": 4, "is_plugin": false, "is_suppressed": false, "exited": true, "tab_id": 0},
          {"id": 5, "is_plugin": false, "is_suppressed": false, "is_floating": true, "tab_id": 0}
        ]"#;
    let parsed: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    let listed: Vec<u64> = parsed
        .iter()
        .filter(|p| p.is_listed_pane())
        .map(|p| p.id)
        .collect();
    let live: Vec<u64> = parsed
        .iter()
        .filter(|p| p.is_live_terminal())
        .map(|p| p.id)
        .collect();
    let tiled: Vec<u64> = parsed
        .iter()
        .filter(|p| p.is_terminal())
        .map(|p| p.id)
        .collect();
    assert_eq!(listed, vec![0, 5]);
    assert_eq!(live, vec![0]);
    assert_eq!(tiled, vec![0, 3, 4]);
}

#[test]
fn pane_listing_admits_floating_agent_panes_but_not_floating_plugins() {
    let json = r#"[
          {
            "id": 1, "is_plugin": false, "tab_id": 0,
            "pane_command": "zsh", "pane_cwd": "/repo/main"
          },
          {
            "id": 2, "is_plugin": false, "is_floating": true,
            "tab_position": 4, "tab_name": "work",
            "terminal_command": "codex", "pane_cwd": "/repo/main"
          },
          {
            "id": 3, "is_plugin": true, "is_floating": true,
            "tab_id": 0, "terminal_command": "codex"
          },
          {
            "id": 4, "is_plugin": false, "is_floating": true, "is_held": true,
            "tab_id": 0, "terminal_command": "claude"
          }
        ]"#;
    let parsed: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    let listing = PaneTopologyCache {
        session_name: "rimz-test".to_owned(),
        produced_at_ms: 1,
        writer: None,
        focused_pane: None,
        clients: None,
        panes: parsed,
    }
    .into_pane_listing("rimz-test".to_owned());

    let pane_ids: Vec<&str> = listing
        .panes
        .iter()
        .map(|pane| pane.pane_id.raw())
        .collect();
    assert_eq!(pane_ids, vec!["terminal_1", "terminal_2"]);
    assert_eq!(listing.panes[1].command, None);
    assert!(listing.panes[1].is_floating);
    assert_eq!(listing.panes[1].spawn_command.as_deref(), Some("codex"));
    assert_eq!(listing.panes[0].cwd.as_deref(), Some("/repo/main"));
    assert_eq!(listing.panes[1].cwd.as_deref(), Some("/repo/main"));
    assert_eq!(listing.panes[1].view_id.as_deref(), Some("tab_4"));
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
    let parsed: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    let anchor = PaneId::from_parts(MuxName::Zellij, "terminal_3");

    assert_eq!(
        floating_panes_in_anchor_view(&parsed, &anchor),
        vec![PaneId::from_parts(MuxName::Zellij, "terminal_26")]
    );
}
#[test]
fn topology_cache_panes_preserve_foreground_and_spawn_commands() {
    let cache: PaneTopologyCache = serde_json::from_str(
        r#"{
          "session_name": "rimz-test",
          "produced_at_ms": 1,
          "panes": [
            {
              "id": 6, "tab_position": 0, "tab_name": "main",
              "pane_x": 0, "pane_columns": 20, "title": "rimz-sidebar",
              "pane_command": "rimz-sidebar", "terminal_command": "rimz sidebar serve"
            },
            {
              "id": 7, "tab_position": 0, "tab_name": "main",
              "is_held": true, "is_focused": true, "pane_x": 20,
              "pane_columns": 100, "title": "claude", "pane_command": "claude",
              "terminal_command": "rimz agents exec claude"
            }
          ]
        }"#,
    )
    .unwrap();
    let panes = cache.panes;

    assert_eq!(panes[0].display_command().as_deref(), Some("rimz-sidebar"));
    assert_eq!(panes[1].foreground_command(), Some("claude"));
    assert_eq!(panes[1].spawn_command(), Some("rimz agents exec claude"));
}

#[test]
fn topology_cache_focus_becomes_authoritative_listing_focus() {
    let cache: PaneTopologyCache = serde_json::from_str(
        r#"{
          "session_name": "rimz-test",
          "produced_at_ms": 1,
          "focused_pane": 7,
          "panes": [
            {"id": 7, "tab_position": 0, "title": "zsh"}
          ]
        }"#,
    )
    .unwrap();

    let listing = cache.into_pane_listing("rimz-test".to_owned());

    assert!(listing.session_focus.is_some());
    assert_eq!(
        listing.session_focus,
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_7"))
    );
}

#[test]
fn sidebar_geometry_classifies_dock_shapes() {
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
           "pane_x": 149, "pane_columns": 149},
          {"id": 7, "is_plugin": false, "tab_id": 3, "title": "rimz-sidebar"},
          {"id": 8, "is_plugin": false, "tab_id": 3, "title": "zsh"},
          {"id": 9, "is_plugin": false, "tab_id": 4, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 10, "is_plugin": false, "tab_id": 4, "title": "codex",
           "pane_x": 60, "pane_columns": 238},
          {"id": 11, "is_plugin": false, "tab_id": 4, "title": "claude",
           "pane_x": 0, "pane_columns": 298},
          {"id": 12, "is_plugin": false, "tab_id": 5, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 13, "is_plugin": false, "tab_id": 5, "title": "codex",
           "pane_x": 60, "pane_columns": 238},
          {"id": 14, "is_plugin": false, "tab_id": 5, "title": "claude",
           "pane_x": 60, "pane_columns": 238},
          {"id": 15, "is_plugin": false, "tab_id": 6, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 16, "is_plugin": false, "tab_id": 6, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 17, "is_plugin": false, "tab_id": 6, "title": "zsh",
           "pane_x": 60, "pane_columns": 238},
          {"id": 18, "is_plugin": false, "tab_id": 7, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 19, "is_plugin": true, "tab_id": 7, "title": "plugin",
           "pane_x": 0, "pane_columns": 298},
          {"id": 20, "is_plugin": false, "is_floating": true, "tab_id": 7, "title": "float",
           "pane_x": 0, "pane_columns": 298},
          {"id": 21, "is_plugin": false, "is_suppressed": true, "tab_id": 7, "title": "suppressed",
           "pane_x": 0, "pane_columns": 298},
          {"id": 22, "is_plugin": false, "tab_id": 7, "title": "zsh",
           "pane_x": 60, "pane_columns": 238},
          {"id": 23, "is_plugin": false, "tab_id": 8, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 24, "is_plugin": false, "tab_id": 8, "title": "codex",
           "pane_x": 60, "pane_columns": 238},
          {"id": 25, "is_plugin": false, "exited": true, "tab_id": 8, "title": "claude",
           "pane_x": 0, "pane_columns": 298},
          {"id": 26, "is_plugin": false, "tab_id": 9, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 60},
          {"id": 27, "is_plugin": false, "tab_id": 9, "title": "codex",
           "pane_x": 60, "pane_columns": 100},
          {"id": 28, "is_plugin": false, "tab_id": 9, "title": "shell",
           "pane_x": 160, "pane_columns": 138},
          {"id": 29, "is_plugin": false, "tab_id": 9, "title": "claude",
           "pane_x": 0, "pane_columns": 298},
          {"id": 30, "is_plugin": false, "tab_id": 10, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 57},
          {"id": 31, "is_plugin": false, "tab_id": 10, "title": "zsh",
           "pane_x": 57, "pane_columns": 241},
          {"id": 32, "is_plugin": false, "tab_id": 11, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 87},
          {"id": 33, "is_plugin": false, "tab_id": 11, "title": "zsh",
           "pane_x": 87, "pane_columns": 211}
        ]"#;
    let panes: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    let target_cols = std::num::NonZeroU16::new(72).expect("nonzero target");
    let target = crate::mux::SidebarTarget {
        share: crate::mux::WidthPermille::from_cols(
            target_cols,
            std::num::NonZeroU16::new(298).expect("nonzero view"),
        ),
        max_cols: target_cols,
        pinned: true,
    };
    let by_id = |id: u64| panes.iter().find(|pane| pane.id == id).unwrap();
    let excluded = HashSet::new();
    assert!(
        sidebar_geometry_off_spec(by_id(2), &panes, &excluded, target),
        "right-docked 50% sidebar is off-spec",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(2), &panes, &excluded),
        Some(SidebarDock::SwapReachable),
    );
    assert!(
        sidebar_geometry_off_spec(by_id(3), &panes, &excluded, target),
        "a layout-born sidebar below the target is repaired",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(3), &panes, &excluded),
        Some(SidebarDock::Docked),
    );
    assert!(
        sidebar_geometry_off_spec(by_id(5), &panes, &excluded, target),
        "left but 50%-wide still wants the resize",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(5), &panes, &excluded),
        Some(SidebarDock::Docked),
        "a wide sidebar with a clear band is docked and only needs resizing",
    );
    assert!(
        !sidebar_geometry_off_spec(by_id(7), &panes, &excluded, target),
        "missing geometry leaves nothing safe to repair",
    );
    assert_eq!(sidebar_dock_verdict(by_id(7), &panes, &excluded), None);
    assert!(
        sidebar_geometry_off_spec(by_id(9), &panes, &excluded, target),
        "the live broken nested-row shape is off-spec",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(9), &panes, &excluded),
        Some(SidebarDock::NestedRow),
    );
    assert_eq!(
        repairable_nested_work_pane_ids(by_id(9), &panes, &excluded),
        Some(vec![10, 11]),
        "the narrow one-right-column nested shape can be repaired by stacking",
    );
    assert!(
        sidebar_geometry_off_spec(by_id(12), &panes, &excluded, target),
        "a docked sidebar below the target still needs resizing",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(12), &panes, &excluded),
        Some(SidebarDock::Docked),
    );
    let excluded_duplicate = HashSet::from([16]);
    assert_eq!(
        sidebar_dock_verdict(by_id(15), &panes, &excluded_duplicate),
        Some(SidebarDock::Docked),
        "a closing duplicate sidebar must not fake a nested-row verdict",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(18), &panes, &excluded),
        Some(SidebarDock::Docked),
        "plugin, floating, and suppressed panes do not intrude into the dock band",
    );
    assert!(
        sidebar_geometry_off_spec(by_id(23), &panes, &excluded, target),
        "a dead pane still means the sidebar is not a full-height dock",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(23), &panes, &excluded),
        Some(SidebarDock::NestedRow),
    );
    assert_eq!(
        repairable_nested_work_pane_ids(by_id(23), &panes, &excluded),
        None,
        "a dead intruder is reportable but not a repair candidate",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(26), &panes, &excluded),
        Some(SidebarDock::NestedRow),
    );
    assert_eq!(
        repairable_nested_work_pane_ids(by_id(26), &panes, &excluded),
        None,
        "multi-column work layouts are left untouched instead of collapsed",
    );
    assert_eq!(
        nested_work_pane_ids(by_id(26), &panes, &excluded),
        Some(vec![28, 27, 29]),
        "a newly added nested sidebar can stack every live work pane without replacing it",
    );
    assert!(
        sidebar_geometry_off_spec(by_id(30), &panes, &excluded, target),
        "a sidebar below the target grows",
    );
    assert!(
        sidebar_geometry_off_spec(by_id(32), &panes, &excluded, target),
        "a sidebar at least one resize step above the target shrinks",
    );
    assert_eq!(
        tab_view_cols(&panes, 7),
        Some(298),
        "only tiled terminals define the tab extent",
    );
    assert_eq!(
        tab_view_cols(&panes, 3),
        None,
        "missing terminal geometry leaves the view width unknown",
    );

    assert!(
        !sidebar_width_off_spec(72, 72, zellij_resize_stop_step_cols(298)),
        "canonical width is not a mis-mount",
    );
    assert!(
        sidebar_width_off_spec(149, 72, zellij_resize_stop_step_cols(298)),
        "the 50% mis-mount is wider than the canonical width",
    );
    assert!(
        sidebar_width_off_spec(60, 72, zellij_resize_stop_step_cols(298)),
        "a pane below the target is repaired",
    );
    assert!(
        sidebar_width_off_spec(90, 72, zellij_resize_stop_step_cols(298)),
        "a pane at least one resize step above the target shrinks",
    );
}

#[test]
fn new_pane_stdout_parses_only_a_bare_terminal_id() {
    assert_eq!(
        parse_new_pane_id(" terminal_58\n"),
        Some(ZellijPaneId::Terminal(58)),
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
fn mounted_sidebar_discovery_prefers_hint_then_new_sidebar() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 7, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 9, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"},
          {"id": 10, "is_plugin": false, "tab_id": 2, "title": "rimz-sidebar"},
          {"id": 11, "is_plugin": false, "tab_id": 2, "title": "vim"}
        ]"#;
    let panes: Vec<PaneTopologyPane> = serde_json::from_str(json).unwrap();
    let before: HashSet<u64> = [1].into();
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, Some(7)), Some(7));
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, None), Some(7));
    assert_eq!(mounted_sidebar_pane(&panes, 0, &before, Some(42)), Some(7));
    assert_eq!(mounted_sidebar_pane(&panes, 3, &before, None), None);
    assert_eq!(mounted_sidebar_pane(&panes, 1, &before, None), Some(9));
    let before_hinted: HashSet<u64> = [7].into();
    assert_eq!(
        mounted_sidebar_pane(&panes, 0, &before_hinted, Some(7)),
        None,
        "a cross-talked hint for an existing sidebar is not a fresh add result",
    );
    let before_existing: HashSet<u64> = [10, 11].into();
    assert_eq!(
        mounted_sidebar_pane(&panes, 2, &before_existing, None),
        None,
        "the tab's only sidebar pane predates the add",
    );
}

#[test]
fn wrong_tab_mount_discovery_handles_missing_and_cross_talked_hints() {
    let panes: Vec<PaneTopologyPane> = serde_json::from_str(
        r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 7, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"}
        ]"#,
    )
    .unwrap();
    let before: HashSet<u64> = [1].into();

    assert_eq!(
        wrong_tab_mounted_sidebar_pane(&panes, 0, &before, Some(7)),
        Some(7)
    );
    assert_eq!(
        wrong_tab_mounted_sidebar_pane(&panes, 0, &before, None),
        Some(7),
        "one fresh wrong-tab sidebar is attributable without stdout",
    );
    assert_eq!(
        wrong_tab_mounted_sidebar_pane(&panes, 0, &before, Some(42)),
        Some(7),
        "a cross-talked hint does not hide one attributable wrong-tab mount",
    );
    let ambiguous: Vec<PaneTopologyPane> = serde_json::from_str(
        r#"[
          {"id": 7, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"},
          {"id": 8, "is_plugin": false, "tab_id": 2, "title": "rimz-sidebar"}
        ]"#,
    )
    .unwrap();
    assert_eq!(
        wrong_tab_mounted_sidebar_pane(&ambiguous, 0, &HashSet::new(), None),
        None,
        "ambiguous concurrent mounts are never closed by guesswork",
    );
}
