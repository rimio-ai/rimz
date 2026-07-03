use super::*;
use std::collections::HashSet;

use crate::ids::{MuxName, PaneId, ViewId, ViewKind};
use crate::mux::zellij::pane_topology::PaneTopologyCache;
use crate::pane::PaneRef;

#[test]
fn raw_pane_position_metadata_accepts_live_and_topology_shapes() {
    let json = r#"[
          {
            "id": 8, "is_plugin": false, "tab_id": 42, "tab_position": 1,
            "tab_name": "rimzd", "pane_x": 60, "pane_columns": 118,
            "title": "claude remote-control --spawn worktree",
            "terminal_command": "claude remote-control --spawn worktree"
          }
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    assert_eq!(parsed[0].tab_id, 42);
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
    let panes = raw_panes_from_topology(cache);
    assert_eq!(panes[0].tab_id, 3);
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
fn views_with_sidebars_classifies_working_orphan_and_daemon_tabs() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 3, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 9, "is_plugin": true,  "tab_id": 0, "title": "zellij:status"},
          {"id": 4, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"},
          {
            "id": 5, "is_plugin": false, "tab_id": 2,
            "title": "/home/me/.cargo/bin/rimz codex app-server serve --workspace-id ws_1 --session-name room",
            "pane_command": "/home/me/.cargo/bin/rimz codex app-server serve --workspace-id ws_1 --session-name room"
          },
          {
            "id": 6, "is_plugin": false, "tab_id": 3, "tab_name": "rimzd",
            "title": "claude"
          },
          {
            "id": 7, "is_plugin": false, "tab_id": 4, "tab_name": "Tab #5",
            "title": "zsh"
          }
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let views = views_with_sidebars(&panes);
    assert_eq!(views.len(), 5);

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

    assert_eq!(views[1].view, "1");
    assert!(!views[1].has_working);
    assert!(!views[1].has_daemon_host);
    assert_eq!(views[1].sidebar_panes.len(), 1);

    for daemon in [&views[2], &views[3]] {
        assert!(!daemon.has_working, "daemon view is not user work");
        assert!(daemon.has_daemon_host, "daemon view must be retained");
        assert!(daemon.sidebar_panes.is_empty(), "daemon host is not chrome");
    }
    assert!(views[4].has_working);
    assert!(!views[4].has_daemon_host);
}

#[test]
fn docked_sidebar_cols_returns_single_live_left_sidebar_width() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72},
          {"id": 2, "is_plugin": false, "tab_id": 0, "title": "zsh",
           "pane_x": 72, "pane_columns": 228}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(docked_sidebar_cols(&panes).map(|cols| cols.get()), Some(72));
}

#[test]
fn docked_sidebar_cols_prefers_majority_width() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72},
          {"id": 2, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 33},
          {"id": 3, "is_plugin": false, "tab_id": 2, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(docked_sidebar_cols(&panes).map(|cols| cols.get()), Some(72));
}

#[test]
fn docked_sidebar_cols_breaks_ties_by_earliest_tab() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 4, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72},
          {"id": 2, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 33}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(docked_sidebar_cols(&panes).map(|cols| cols.get()), Some(33));
}

#[test]
fn docked_sidebar_cols_filters_non_live_or_non_docked_panes() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar",
           "pane_x": 72, "pane_columns": 72},
          {"id": 2, "is_plugin": false, "exited": true, "tab_id": 1,
           "title": "rimz-sidebar", "pane_x": 0, "pane_columns": 72},
          {"id": 3, "is_plugin": true, "tab_id": 2, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72},
          {"id": 4, "is_plugin": false, "tab_id": 3, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 0},
          {"id": 5, "is_plugin": false, "tab_id": 4, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 70000},
          {"id": 6, "is_plugin": false, "tab_id": 5, "title": "zsh",
           "pane_x": 0, "pane_columns": 72},
          {"id": 7, "is_plugin": false, "tab_id": 6, "title": "rimz-sidebar",
           "pane_x": 0, "pane_columns": 72},
          {"id": 8, "is_plugin": false, "tab_id": 6, "title": "zsh",
           "pane_x": 0, "pane_columns": 72}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();

    assert_eq!(docked_sidebar_cols(&panes), None);
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
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
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
            "tab_id": 0, "tab_position": 4, "tab_name": "work",
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
    let parsed: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let listing = RawPaneListing::from_cli(parsed, 1, None).into_pane_listing(
        "rimz-test".to_owned(),
        |mut p, session_name| {
            if !p.is_listed_pane() {
                return None;
            }
            let command = p.display_command();
            Some(PaneRef {
                pane_id: PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", p.id)),
                session_name: session_name.to_owned(),
                view_id: Some(format!("tab_{}", p.view_position())),
                view_kind: Some(ViewKind::Tab),
                view_name: p.tab_name.take(),
                is_focused: p.is_focused,
                is_floating: p.is_floating,
                pane_pid: p.pid(),
                pane_process_start: p.process_start(),
                hosted_agent_kind: None,
                hosted_agent_process_start: None,
                command,
                spawn_command: p.spawn_command().map(str::to_owned),
                cwd: p.reported_cwd().map(str::to_owned),
                resumed_session_id: None,
                elevated_agent: None,
                first_seen_at_ms: None,
            })
        },
    );

    let pane_ids: Vec<&str> = listing
        .panes
        .iter()
        .map(|pane| pane.pane_id.raw())
        .collect();
    assert_eq!(pane_ids, vec!["terminal_1", "terminal_2"]);
    assert_eq!(listing.panes[1].command, None);
    assert!(listing.panes[1].is_floating);
    assert_eq!(listing.panes[1].spawn_command.as_deref(), Some("codex"));
    assert_eq!(listing.panes[1].cwd.as_deref(), Some("/repo/main"));
    assert_eq!(listing.panes[1].view_id.as_deref(), Some("tab_4"));
}

#[test]
fn cli_source_active_uses_first_focused_listed_pane_per_tab() {
    let json = r#"[
          {"id": 12, "is_plugin": false, "is_focused": true, "tab_id": 7, "tab_position": 2},
          {"id": 9, "is_plugin": false, "is_focused": true, "tab_id": 7, "tab_position": 2},
          {"id": 21, "is_plugin": false, "is_focused": true, "tab_id": 8, "tab_position": 3},
          {"id": 22, "is_plugin": false, "is_focused": false, "tab_id": 8, "tab_position": 3}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let listing = RawPaneListing::from_cli(panes, 1, None);

    assert!(!listing.source_active_authoritative);
    assert_eq!(
        listing.source_active.get(&ViewId::new_unchecked("tab_2")),
        Some(&PaneId::from_parts(MuxName::Zellij, "terminal_12")),
        "the first focused pane in CLI order wins a multi-focus tab",
    );
    assert_eq!(
        listing.source_active.get(&ViewId::new_unchecked("tab_3")),
        Some(&PaneId::from_parts(MuxName::Zellij, "terminal_21")),
    );
}

#[test]
fn cli_source_active_excludes_focused_unlisted_panes() {
    let json = r#"[
          {"id": 30, "is_plugin": true, "is_focused": true, "tab_id": 1},
          {"id": 31, "is_plugin": false, "is_focused": true, "tab_id": 1},
          {"id": 40, "is_plugin": true, "is_focused": true, "tab_id": 2},
          {"id": 50, "is_plugin": false, "is_focused": true, "is_held": true, "tab_id": 3}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let listing = RawPaneListing::from_cli(panes, 1, None);

    assert!(!listing.source_active_authoritative);
    assert_eq!(
        listing.source_active.get(&ViewId::new_unchecked("tab_1")),
        Some(&PaneId::from_parts(MuxName::Zellij, "terminal_31")),
        "a focused plugin never wins over a listed terminal",
    );
    assert!(
        !listing
            .source_active
            .contains_key(&ViewId::new_unchecked("tab_2"))
    );
    assert!(
        !listing
            .source_active
            .contains_key(&ViewId::new_unchecked("tab_3"))
    );
}

#[test]
fn cli_source_active_uses_plugin_active_panes_when_available() {
    let json = r#"[
          {"id": 79, "is_plugin": false, "is_focused": true, "tab_id": 7, "tab_position": 1},
          {"id": 200, "is_plugin": false, "is_focused": true, "tab_id": 7, "tab_position": 1}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let listing =
        RawPaneListing::from_cli(panes, 1, Some(std::collections::BTreeMap::from([(1, 200)])));

    assert!(listing.source_active_authoritative);
    assert!(!listing.served_from_topology);
    assert_eq!(
        listing.source_active.get(&ViewId::new_unchecked("tab_1")),
        Some(&PaneId::from_parts(MuxName::Zellij, "terminal_200")),
    );
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

    let suspended_floating_command = r#"[
          {"id": 0, "is_plugin": false, "title": "rimz-sidebar", "is_held": false, "tab_id": 0},
          {
            "id": 1, "is_plugin": false, "is_floating": true,
            "title": "codex", "is_held": true, "tab_id": 0
          }
        ]"#;
    let parsed: Vec<RawPane> = serde_json::from_str(suspended_floating_command).unwrap();
    assert_eq!(
        classify_session_panes(&parsed),
        SessionCleanliness::SuspendedCommandPane,
    );
}
#[test]
fn topology_cache_panes_feed_the_existing_classifier() {
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
           "pane_x": 0, "pane_columns": 298}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
    let canonical_cols = 72;
    let by_id = |id: u64| panes.iter().find(|pane| pane.id == id).unwrap();
    let excluded = HashSet::new();
    assert!(
        sidebar_geometry_off_spec(by_id(2), &panes, &excluded, canonical_cols),
        "right-docked 50% sidebar is off-spec",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(2), &panes, &excluded),
        Some(SidebarDock::SwapReachable),
    );
    assert!(
        !sidebar_geometry_off_spec(by_id(3), &panes, &excluded, canonical_cols),
        "a healthy ~21% layout-born sidebar is never churned",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(3), &panes, &excluded),
        Some(SidebarDock::Docked),
    );
    assert!(
        sidebar_geometry_off_spec(by_id(5), &panes, &excluded, canonical_cols),
        "left but 50%-wide still wants the resize",
    );
    assert_eq!(
        sidebar_dock_verdict(by_id(5), &panes, &excluded),
        Some(SidebarDock::Docked),
        "a wide sidebar with a clear band is docked and only needs resizing",
    );
    assert!(
        !sidebar_geometry_off_spec(by_id(7), &panes, &excluded, canonical_cols),
        "missing geometry leaves nothing safe to repair",
    );
    assert_eq!(sidebar_dock_verdict(by_id(7), &panes, &excluded), None);
    assert!(
        sidebar_geometry_off_spec(by_id(9), &panes, &excluded, canonical_cols),
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
        !sidebar_geometry_off_spec(by_id(12), &panes, &excluded, canonical_cols),
        "a sidebar beside stacked right-hand work panes is docked",
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
        sidebar_geometry_off_spec(by_id(23), &panes, &excluded, canonical_cols),
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

    assert!(
        !sidebar_width_off_spec(canonical_cols, canonical_cols),
        "canonical width is not a mis-mount",
    );
    assert!(
        sidebar_width_off_spec(149, canonical_cols),
        "the 50% mis-mount is wider than the canonical width",
    );
    assert!(
        !sidebar_width_off_spec(60, canonical_cols),
        "sub-canonical panes are left untouched until the view reopens",
    );
    assert!(
        sidebar_width_off_spec(90, canonical_cols),
        "anything wider than canonical still shrinks",
    );
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
fn mounted_sidebar_discovery_prefers_hint_then_new_sidebar() {
    let json = r#"[
          {"id": 1, "is_plugin": false, "tab_id": 0, "title": "zsh"},
          {"id": 7, "is_plugin": false, "tab_id": 0, "title": "rimz-sidebar"},
          {"id": 9, "is_plugin": false, "tab_id": 1, "title": "rimz-sidebar"},
          {"id": 10, "is_plugin": false, "tab_id": 2, "title": "rimz-sidebar"},
          {"id": 11, "is_plugin": false, "tab_id": 2, "title": "vim"}
        ]"#;
    let panes: Vec<RawPane> = serde_json::from_str(json).unwrap();
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
