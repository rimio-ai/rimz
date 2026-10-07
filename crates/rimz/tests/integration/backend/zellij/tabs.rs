use std::path::Path;
use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::tab_name::TabNameIntent;
use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, TabOptions, ZellijBackend};
use tempfile::TempDir;

use super::support::*;
use crate::common::CommandTimeoutExt;

#[test]
fn tab_injects_env_into_every_column_and_row() {
    require_zellij!();
    let room = LiveZellijSession::new("tab-env");
    let cwd = TempDir::new().unwrap();
    room.create_plain_background(cwd.path(), "60");
    wait_for_pane_count(room.path(), room.name(), 1);
    let _client = AttachedClient::attach(&room, 200, 60);
    let (_stub_dir, stub) = sidebar_stub_alive_for(60);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        session_name: room.name().to_owned(),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_owned(),
        extra_env: Default::default(),
        cwd: cwd.path().to_owned(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(50).unwrap(),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: Default::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let columns = (0..2)
        .map(|column| {
            tiled_column(
                (0..2)
                    .map(|row| PaneCmd {
                        argv: vec![
                            "sh".into(),
                            "-c".into(),
                            "printf '%s' \"$RIMZ_TEST_VAR\" > \"$1\"; exec sleep 60".into(),
                            "marker".into(),
                            format!("marker-{column}-{row}"),
                        ],
                        name: None,
                    })
                    .collect(),
            )
        })
        .collect();
    room.backend()
        .open_tab(&TabOptions {
            env: std::collections::BTreeMap::from([(
                "RIMZ_TEST_VAR".into(),
                "marker with spaces".into(),
            )]),
            title: "env".into(),
            panes: LayoutPanes {
                columns,
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: false,
            after: None,
            sidebar,
        })
        .unwrap();
    for column in 0..2 {
        for row in 0..2 {
            let marker = cwd.path().join(format!("marker-{column}-{row}"));
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while !std::fs::read_to_string(&marker).is_ok_and(|text| !text.is_empty()) {
                assert!(
                    std::time::Instant::now() < deadline,
                    "missing {}",
                    marker.display()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(
                std::fs::read_to_string(marker).unwrap(),
                "marker with spaces"
            );
        }
    }
    let runtime =
        rimz::RuntimePaths::under(WorkspaceId::from_project_root(cwd.path()), room.path()).unwrap();
    let file: serde_json::Value = std::fs::read(runtime.lane_path("tab-owners.json"))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .unwrap_or_default();
    let panes = expect_list_panes(room.path(), room.name());
    let founders = panes
        .panes
        .iter()
        .filter(|pane| {
            !pane.is_plugin && !pane.is_sidebar() && pane.tab_name.as_deref() == Some("env")
        })
        .map(|pane| PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id)))
        .collect::<Vec<_>>();
    let tab = panes
        .panes
        .iter()
        .find(|pane| pane.tab_name.as_deref() == Some("env"))
        .unwrap()
        .tab_id;
    let owner: Option<rimz::mux::tab_name::TabOwnerRecord> =
        serde_json::from_value(file["tabs"][tab.to_string()].clone()).unwrap();
    assert_eq!(
        owner,
        Some(rimz::mux::tab_name::TabOwnerRecord {
            base: "env".to_owned(),
            founders
        })
    );
}

#[test]
fn companion_grid_preserves_processes_sidebar_and_focus() {
    use rimz::mux::{CompanionPaneAppend, SplitPaneOptions, SplitTarget};

    require_zellij!();
    let room = LiveZellijSession::new("companion-grid");
    let backend = room.backend();
    let version = backend.version().expect("zellij version");
    let minor = version
        .split('.')
        .nth(1)
        .and_then(|value| value.parse::<u32>().ok());
    if minor.is_none_or(|minor| minor < 45) {
        crate::common::skip("zellij below 0.45");
        return;
    }
    let cwd = TempDir::new().expect("cwd");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        session_name: room.name().to_owned(),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(50).expect("width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: Default::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    publish_room_bin(room.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("sidebar");
    wait_for_pane_count(room.path(), room.name(), 2);
    let mut client = AttachedClient::attach(&room, 300, 100);
    let source = expect_list_panes(room.path(), room.name())
        .panes
        .into_iter()
        .find(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .expect("source");
    let source = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", source.id));
    client.wait_until_focused(&source, "source before companion");
    let command = |index: usize| {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf '%s\\n' \"$$\" > \"$1\"; exec sleep 600".to_owned(),
            "grid".to_owned(),
            cwd.path()
                .join(format!("pid-{index}"))
                .display()
                .to_string(),
        ]
    };
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "companion".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: command(1),
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .expect("companion tab");
    let first = wait_for_named_work_pane_count(room.path(), room.name(), "companion", 1)[0];
    let anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", first.id));
    let initial = expect_list_panes(room.path(), room.name());
    let chrome = initial
        .panes
        .iter()
        .filter(|pane| pane.is_plugin || pane.is_sidebar())
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>();
    let mut identities = Vec::new();
    let mut ids = vec![first.id];
    for count in 1_usize..=8 {
        if count > 1 {
            assert_eq!(
                backend
                    .append_companion_pane(SplitPaneOptions {
                        target: SplitTarget::SessionPane {
                            session_name: room.name().to_owned(),
                            pane_id: anchor.clone()
                        },
                        command: Some(command(count)),
                        ..Default::default()
                    })
                    .expect("append companion"),
                CompanionPaneAppend::Opened,
                "pane {count}"
            );
        }
        let panes = wait_for_named_work_pane_count(room.path(), room.name(), "companion", count);
        assert!(ids.iter().all(|id| panes.iter().any(|pane| pane.id == *id)));
        ids = panes.iter().map(|pane| pane.id).collect();
        let snapshot = expect_list_panes(room.path(), room.name());
        assert_eq!(
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.is_plugin || pane.is_sidebar())
                .map(|pane| pane.geometry())
                .collect::<Vec<_>>(),
            chrome
        );
        let pid = poll_until(
            Duration::from_secs(5),
            || {
                std::fs::read_to_string(cwd.path().join(format!("pid-{count}")))
                    .map_err(|err| err.to_string())
                    .and_then(|text| text.trim().parse::<u32>().map_err(|err| err.to_string()))
            },
            |_| true,
            "child pid",
        );
        identities.push((
            pid,
            rimz::proc::process_start_token(pid).expect("process start"),
        ));
        for (pid, start) in &identities {
            assert!(
                rimz::proc::process_is_live(*pid, Some(start)),
                "process {pid} replaced"
            );
        }
        let mut bands = std::collections::BTreeMap::<u64, Vec<&PaneGeometry>>::new();
        for pane in &panes {
            bands.entry(pane.x).or_default().push(pane);
        }
        assert!(
            bands.len() == count.min(2)
                && bands.values().all(|band| band.len() <= count.div_ceil(2)),
            "grid bounds at {count} panes: {panes:?}"
        );
        let spread = |sizes: &[u64]| {
            sizes.iter().max().expect("max size") - sizes.iter().min().expect("min size")
        };
        let widths = bands
            .values()
            .map(|band| band[0].columns)
            .collect::<Vec<_>>();
        assert!(
            spread(&widths) <= 1,
            "equal column widths at {count} panes: {panes:?}"
        );
        for band in bands.values() {
            let rows = band.iter().map(|pane| pane.rows).collect::<Vec<_>>();
            // Native resizes move 5% of the tab, so rows settle within one step.
            let step = rows.iter().sum::<u64>().div_ceil(20);
            assert!(
                spread(&rows) <= step,
                "equal heights within one native step at {count} panes: {panes:?}"
            );
        }
        if count == 8 {
            assert!(bands.values().all(|band| band.len() == 4));
        }
        client.assert_input_reaches(&source, "source while appending companion");
    }
    assert_eq!(
        backend
            .append_companion_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: room.name().to_owned(),
                    pane_id: anchor
                },
                command: Some(command(9)),
                ..Default::default()
            })
            .expect("full companion"),
        CompanionPaneAppend::Full
    );
    assert!(!cwd.path().join("pid-9").exists());
    assert_eq!(
        wait_for_named_work_pane_count(room.path(), room.name(), "companion", 8).len(),
        8
    );
}

#[test]
fn directional_background_split_uses_exact_anchor_rectangle() {
    use rimz::mux::{SplitDirection, SplitPaneOptions, SplitPlacement, SplitTarget};

    require_zellij!();
    let room = LiveZellijSession::new("exact-split-anchor");
    room.create_background();
    let backend = room.backend();
    let version = backend.version().expect("version");
    if version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .is_none_or(|minor| minor < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    let mut client = AttachedClient::attach(&room, 200, 80);
    let source = expect_list_panes(room.path(), room.name())
        .panes
        .into_iter()
        .find(|pane| pane.is_live_terminal())
        .expect("source");
    let anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", source.id));
    let split = |pane_id, direction| SplitPaneOptions {
        target: SplitTarget::SessionPane {
            session_name: room.name().to_owned(),
            pane_id,
        },
        placement: SplitPlacement::Directional(direction),
        command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
        focus: false,
        ..Default::default()
    };
    backend
        .split_pane(split(anchor.clone(), SplitDirection::Right))
        .expect("first split");
    let before = poll_until(
        Duration::from_secs(5),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.is_live_terminal())
                .count()
                == 2
        },
        "first split",
    );
    let other = before
        .panes
        .iter()
        .find(|pane| pane.is_live_terminal() && pane.id != source.id)
        .expect("right pane")
        .geometry();
    let target = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", other.id));
    backend
        .split_pane(split(target, SplitDirection::Down))
        .expect("split unfocused right pane");
    let after = poll_until(
        Duration::from_secs(5),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.is_live_terminal())
                .count()
                == 3
        },
        "exact split",
    );
    assert_eq!(
        before
            .panes
            .iter()
            .find(|pane| pane.id == source.id && !pane.is_plugin)
            .unwrap()
            .geometry(),
        after
            .panes
            .iter()
            .find(|pane| pane.id == source.id && !pane.is_plugin)
            .unwrap()
            .geometry()
    );
    let right = after
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal() && pane.id != source.id)
        .collect::<Vec<_>>();
    assert_eq!(right.len(), 2);
    assert!(
        right
            .iter()
            .all(|pane| pane.pane_x == other.x && pane.pane_columns == other.columns)
    );
    assert_eq!(
        right.iter().map(|pane| pane.pane_rows).sum::<u64>(),
        other.rows
    );
    client.assert_input_reaches(&anchor, "original focus after exact anchor split");
}

#[test]
fn directional_focus_split_divides_its_anchor_not_the_focused_pane() {
    use rimz::mux::{SplitDirection, SplitPaneOptions, SplitPlacement, SplitTarget};

    require_zellij!();
    let room = LiveZellijSession::new("focus-split-anchor");
    room.create_background();
    let backend = room.backend();
    let version = backend.version().expect("version");
    if version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .is_none_or(|minor| minor < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    // 80 pane rows halve twice without a remainder, so Zellij has no odd row
    // to hand to a neighbour and every untouched pane compares exactly.
    let mut client = AttachedClient::attach(&room, 200, 82);
    let live = |count: usize, context: &str| {
        poll_until(
            Duration::from_secs(5),
            || list_panes(room.path(), room.name()),
            |snapshot| {
                snapshot
                    .panes
                    .iter()
                    .filter(|pane| pane.is_live_terminal())
                    .count()
                    == count
            },
            context,
        )
        .panes
        .into_iter()
        .filter(|pane| pane.is_live_terminal())
        .map(|pane| pane.geometry())
        .collect::<Vec<_>>()
    };
    let pane_id = |id: u64| PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}"));
    let split = |pane_id, direction, focus| SplitPaneOptions {
        target: SplitTarget::SessionPane {
            session_name: room.name().to_owned(),
            pane_id,
        },
        placement: SplitPlacement::Directional(direction),
        command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
        focus,
        ..Default::default()
    };
    let bystander = live(1, "birth pane")[0];
    backend
        .split_pane(split(pane_id(bystander.id), SplitDirection::Right, false))
        .expect("anchor pane");
    let mut before = live(2, "anchor pane");
    // A client that has sent input is the one a focus-taking spawn follows.
    client.assert_input_reaches(&pane_id(bystander.id), "bystander before the focus split");
    let bystander = *before
        .iter()
        .find(|pane| pane.id == bystander.id)
        .expect("bystander");
    let anchor = *before
        .iter()
        .find(|pane| pane.id != bystander.id)
        .expect("anchor");

    // Twice against one anchor: the shape of a lane resume restoring two agents.
    for round in 1..=2 {
        backend
            .split_pane(split(pane_id(anchor.id), SplitDirection::Down, true))
            .expect("focus-taking split");
        let after = live(before.len() + 1, "focus-taking split");
        let created = *after
            .iter()
            .find(|pane| before.iter().all(|known| known.id != pane.id))
            .expect("created pane");
        let halved = before
            .iter()
            .find(|pane| pane.id == anchor.id)
            .expect("anchor before");
        let now = after
            .iter()
            .find(|pane| pane.id == anchor.id)
            .expect("anchor after");
        assert_eq!(
            (created.x, created.columns, now.rows + created.rows),
            (halved.x, halved.columns, halved.rows),
            "round {round}: the new pane takes its rows from the anchor: {after:?}"
        );
        for untouched in before.iter().filter(|pane| pane.id != anchor.id) {
            assert!(
                after.contains(untouched),
                "round {round}: pane {untouched:?} moved: {after:?}"
            );
        }
        client.assert_input_reaches(&pane_id(created.id), "pane created by the focus split");
        before = after;
    }
}

#[test]
fn rename_tab_uses_the_anchor_panes_stable_tab_id() {
    require_zellij!();

    let room = LiveZellijSession::new("rename-tab");
    let (_stub_dir, stub) = sidebar_command_stub();
    let mut opts = sidebar_opts(room.name(), room.path(), stub, 100);
    opts.workspace_id = WorkspaceId::from_project_root(room.path());
    publish_room_bin(room.path(), &opts);
    let runtime = rimz::RuntimePaths::under(opts.workspace_id.clone(), room.path()).unwrap();
    room.create_background();
    let mut client = AttachedClient::attach(&room, 100, 30);
    open_new_tab(room.path(), room.name());
    open_new_tab(room.path(), room.name());
    let before = expect_list_panes(room.path(), room.name());
    let ids = before.tab_ids();
    assert_eq!(
        ids.len(),
        3,
        "fixture should start with three tabs: {before:?}"
    );
    let removed = ids[1];
    let target_id = ids[2];
    let output = room
        .command()
        .args([
            "--session",
            room.name(),
            "action",
            "close-tab-by-id",
            &removed.to_string(),
        ])
        .bounded_output()
        .expect("close middle tab");
    assert!(
        output.status.success(),
        "close middle tab failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let shifted = poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |snapshot| snapshot.tab_ids() == vec![ids[0], target_id],
        "middle tab close",
    );
    let target = shifted
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == target_id)
        .expect("target tab work pane");
    assert_ne!(
        target.tab_position,
        Some(target_id),
        "fixture must distinguish stable id from shifted position",
    );
    let anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", target.id));
    let other = shifted
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == ids[0])
        .expect("other tab work pane");
    let other_anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", other.id));
    client.go_to_tab_until(1, &other_anchor, "unrelated pane before claim");

    room.backend()
        .rename_tab(
            room.name(),
            &anchor,
            "#feat",
            TabNameIntent::Claim {
                pane_name: "opus".to_owned(),
            },
        )
        .expect("name existing launch tab");
    let file: serde_json::Value = std::fs::read(runtime.lane_path("tab-owners.json"))
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
        .unwrap_or_default();
    assert_eq!(file["tabs"][target_id.to_string()]["base"], "#feat");
    assert_eq!(
        file["tabs"][target_id.to_string()]["founders"],
        serde_json::json!([anchor])
    );
    poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == target_id)
                .all(|pane| pane.tab_name.as_deref() == Some("#feat"))
                && snapshot.panes.iter().any(|pane| {
                    pane.id == target.id && !pane.is_plugin && pane.title.as_deref() == Some("opus")
                })
        },
        "launch title replaces default tab name",
    );
    let claimed = expect_list_panes(room.path(), room.name());
    assert_eq!(
        claimed
            .panes
            .iter()
            .find(|pane| !pane.is_plugin && pane.id == other.id)
            .expect("other pane after claim")
            .title,
        other.title
    );

    // A projection from a frame older than the claim finds the tab renamed and
    // leaves it; the Status below, projected from the claimed name, then applies.
    room.backend()
        .rename_tab(
            room.name(),
            &anchor,
            "shell",
            TabNameIntent::Rest {
                observed: "shell ?".to_owned(),
            },
        )
        .expect("stale projection after the claim");
    assert!(
        expect_list_panes(room.path(), room.name())
            .panes
            .iter()
            .filter(|pane| pane.tab_id == target_id)
            .all(|pane| pane.tab_name.as_deref() == Some("#feat")),
        "a stale projection must keep the claimed name",
    );

    room.backend()
        .rename_tab(
            room.name(),
            &anchor,
            "work ✓",
            TabNameIntent::Status {
                observed: "#feat".to_owned(),
            },
        )
        .expect("rename shifted tab by its pane anchor");

    let renamed = poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == target_id)
                .all(|pane| pane.tab_name.as_deref() == Some("work ✓"))
        },
        "renamed stable-id tab",
    );
    assert!(
        renamed
            .panes
            .iter()
            .filter(|pane| pane.tab_id != target_id)
            .all(|pane| pane.tab_name.as_deref() != Some("work ✓")),
        "rename should not affect another tab: {renamed:?}",
    );
    let shell = rimz::proc::shell_pane_name();
    room.backend()
        .rename_tab(
            room.name(),
            &anchor,
            &shell,
            TabNameIntent::Release {
                observed: "work ✓".to_owned(),
            },
        )
        .expect("release anchored tab");
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(runtime.lane_path("tab-owners.json")).unwrap())
            .unwrap();
    assert!(file["tabs"].get(target_id.to_string()).is_none());
    let released = poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot.panes.iter().any(|pane| {
                pane.id == target.id
                    && !pane.is_plugin
                    && pane.tab_name.as_deref() == Some(shell.as_str())
            })
        },
        "released stable-id tab",
    );
    for pane in &renamed.panes {
        let after = released
            .panes
            .iter()
            .find(|after| after.id == pane.id && after.is_plugin == pane.is_plugin)
            .expect("pane survives release");
        assert_eq!(after.title, pane.title, "release must retain pane names");
        if pane.tab_id == target_id {
            assert_eq!(after.tab_name.as_deref(), Some(shell.as_str()));
        } else {
            assert_eq!(
                after.tab_name, pane.tab_name,
                "release must not rename another tab"
            );
        }
    }
}

#[test]
fn open_tab_unfocused_routes_input_back_to_source() {
    require_zellij!();

    let room = LiveZellijSession::new("tabfocus");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-tabfocus"));
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(workspace_id.clone(), cwd.path()).expect("fixture state"),
            cwd.path(),
        ),
        session_name: name.clone(),
        workspace_id,
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(50).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);

    let mut client = AttachedClient::attach(&room, 200, 50);

    let source_tab = "focus source";
    let input_log = cwd.path().join("source-input.log");
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: source_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        format!(
                            "while IFS= read -r line; do printf '%s\\n' \"$line\" >> '{}'; done",
                            input_log.display(),
                        ),
                    ],
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar.clone(),
        })
        .expect("open focused source tab");

    let source_panes = wait_for_named_work_pane_count(xdg, &name, source_tab, 1);
    assert_eq!(
        source_panes.len(),
        1,
        "source tab should have one work pane: {source_panes:?}",
    );
    let source_pane =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", source_panes[0].id));
    let focused = client.wait_until_focused(&source_pane, "focused source tab");
    assert_eq!(
        focused,
        vec![source_pane.clone()],
        "the attached client should focus the source tab before the regression step: {focused:?}",
    );

    let background_tab = "background run";
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: background_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(
                    ["worker", "leader"]
                        .into_iter()
                        .map(|name| PaneCmd {
                            argv: vec!["sleep".to_owned(), "600".to_owned()],
                            name: Some(name.to_owned()),
                        })
                        .collect(),
                )],
                focused_pane: 1,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .expect("open unfocused background tab");
    assert_eq!(
        wait_for_named_work_pane_count(xdg, &name, background_tab, 2).len(),
        2,
        "background tab should open two work panes",
    );

    client.assert_input_reaches(&source_pane, "source pane after unfocused tab open");
    let leader = expect_list_panes(xdg, &name)
        .panes
        .into_iter()
        .find(|pane| pane.title.as_deref() == Some("leader"))
        .expect("background leader");
    let output = crate::common::ZellijNamespace::command_at(xdg)
        .args([
            "--session",
            &name,
            "action",
            "go-to-tab-name",
            background_tab,
        ])
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    client.wait_until_focused(
        &PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", leader.id)),
        "leader on first entry to background tab",
    );
}

#[test]
fn open_tab_after_anchor_inserts_next_to_it() {
    require_zellij!();

    let room = LiveZellijSession::new("tab-anchor");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(
                WorkspaceId::from_project_root(Path::new("/tmp/rimz-tab-anchor")),
                cwd.path(),
            )
            .expect("fixture state"),
            cwd.path(),
        ),
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-tab-anchor")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(50).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);
    let client = AttachedClient::attach(&room, 160, 40);
    let anchor_pane = expect_list_panes(xdg, &name)
        .panes
        .into_iter()
        .filter(|pane| pane.is_live_terminal() && !pane.is_sidebar())
        .min_by_key(|pane| pane.tab_position.unwrap_or(pane.tab_id))
        .expect("anchor work pane");
    let anchor_position = anchor_pane.tab_position.unwrap_or(anchor_pane.tab_id);
    let anchor = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", anchor_pane.id));
    let pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };
    for title in ["middle", "tail"] {
        backend
            .open_tab(&TabOptions {
                env: Default::default(),
                title: title.to_owned(),
                panes: LayoutPanes {
                    columns: vec![tiled_column(vec![pane()])],
                    focused_pane: 0,
                },
                focus: true,
                dock_sidebar: true,
                after: None,
                sidebar: sidebar.clone(),
            })
            .expect("append fixture tab");
    }
    wait_for_tab_count(xdg, &name, 3);
    let tail = wait_for_named_work_pane_count(xdg, &name, "tail", 1)[0];
    let tail = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", tail.id));
    client.wait_until_focused(&tail, "tail before anchored open");

    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "inserted".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![pane()])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: Some(anchor),
            sidebar,
        })
        .expect("open anchored tab");
    wait_for_named_work_pane_count(xdg, &name, "inserted", 1);
    client.wait_until_focused(&tail, "tail after anchored unfocused open");

    let snapshot = expect_list_panes(xdg, &name);
    let position = |tab_name: &str| {
        snapshot
            .panes
            .iter()
            .find(|pane| pane.tab_name.as_deref() == Some(tab_name) && pane.is_live_terminal())
            .map(|pane| pane.tab_position.unwrap_or(pane.tab_id))
            .unwrap_or_else(|| panic!("missing tab {tab_name}: {snapshot:?}"))
    };
    assert_eq!(position("inserted"), anchor_position + 1, "{snapshot:?}");
    assert_eq!(position("middle"), anchor_position + 2, "{snapshot:?}");
    assert_eq!(position("tail"), anchor_position + 3, "{snapshot:?}");
}

#[test]
fn open_tab_can_omit_sidebar_for_gallery_layout() {
    require_zellij!();

    let room = LiveZellijSession::new("gallery");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(
                WorkspaceId::from_project_root(Path::new("/tmp/rimz-gallery")),
                cwd.path(),
            )
            .expect("fixture state"),
            cwd.path(),
        ),
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-gallery")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(55).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);
    let _client = AttachedClient::attach(&room, 220, 40);

    let tab_name = "sidebar gallery";
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: tab_name.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: false,
            after: None,
            sidebar,
        })
        .expect("open gallery tab");

    assert_eq!(
        wait_for_named_work_pane_count(xdg, &name, tab_name, 1).len(),
        1,
        "gallery tab should hold one work pane",
    );
    assert_eq!(
        named_sidebar_pane_geometry(xdg, &name, tab_name)
            .expect("list gallery sidebar panes")
            .map(|pane| pane.id),
        None,
        "gallery tab should not carry a rimz-sidebar pane",
    );
}

/// A native no-direction pane open splits the focused work pane without
/// changing the backend-created tab's docked sidebar.
#[test]
fn native_focused_split_preserves_docked_sidebar() {
    require_zellij!();

    let room = LiveZellijSession::new("worksplit");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(
                WorkspaceId::from_project_root(Path::new("/tmp/rimz-worksplit")),
                cwd.path(),
            )
            .expect("fixture state"),
            cwd.path(),
        ),
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-worksplit")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(75).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);

    let client_columns: u16 = 380;
    let client_rows: u16 = 46;
    let mut client = AttachedClient::attach(&room, client_columns, client_rows);
    write_topology_cache_from_list_panes(xdg, &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg, &sidebar.workspace_id, &name);
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };

    let split_tab = "backend focused split";
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: split_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                ],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar.clone(),
        })
        .expect("open backend split tab layout");
    let work = wait_for_named_work_pane_state(xdg, &name, split_tab, 3, |work| {
        work.iter().map(|pane| pane.x + pane.columns).max() == Some(u64::from(client_columns))
    });
    let focused_before = work[1];
    let focused_before_id =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", focused_before.id));
    client.press_alt_until(
        'l',
        &focused_before_id,
        "chosen work pane before native split",
    );
    let focused = client.wait_until_focused(&focused_before_id, "chosen work pane");
    assert!(
        focused.iter().any(|pane| pane == &focused_before_id),
        "backend tab should focus the chosen work pane before native split; \
         focused client panes: {focused:?}",
    );
    let sidebar_before =
        wait_for_named_sidebar_pane(xdg, &name, split_tab).expect("backend tab keeps its sidebar");
    assert_eq!(
        sidebar_before.x, 0,
        "backend tab starts with the sidebar docked left: {sidebar_before:?}",
    );

    spawn_sleep_pane(xdg, &name, cwd.path());
    let focused_bounds_hold_two_panes = |pane: &PaneGeometry| {
        pane.x + 2 >= focused_before.x
            && pane.y + 2 >= focused_before.y
            && pane.x + pane.columns <= focused_before.x + focused_before.columns + 2
            && pane.y + pane.rows <= focused_before.y + focused_before.rows + 2
    };
    let split = wait_for_named_work_pane_state(xdg, &name, split_tab, 4, |work| {
        let work_stays_right_of_sidebar = work
            .iter()
            .all(|pane| pane.x >= sidebar_before.x + sidebar_before.columns);
        let focused_pane_was_split = work
            .iter()
            .filter(|pane| focused_bounds_hold_two_panes(pane))
            .count()
            == 2;
        work_stays_right_of_sidebar && focused_pane_was_split
    });
    poll_until(
        Duration::from_secs(30),
        || named_sidebar_pane_geometry(xdg, &name, split_tab),
        |sidebar| {
            sidebar.is_some_and(|sidebar| {
                sidebar.x == sidebar_before.x
                    && sidebar.y == sidebar_before.y
                    && sidebar.columns == sidebar_before.columns
                    && sidebar.rows == sidebar_before.rows
            })
        },
        &format!("unchanged sidebar geometry in {name}/{split_tab}"),
    );
    assert_eq!(
        split
            .iter()
            .filter(|pane| focused_bounds_hold_two_panes(pane))
            .count(),
        2,
        "native split should divide only the focused pane, got {split:?}",
    );
    let sidebar_after = named_sidebar_pane_geometry(xdg, &name, split_tab)
        .expect("list backend tab sidebar")
        .expect("backend tab keeps its sidebar");
    assert_eq!(
        (
            sidebar_after.x,
            sidebar_after.y,
            sidebar_after.columns,
            sidebar_after.rows,
        ),
        (
            sidebar_before.x,
            sidebar_before.y,
            sidebar_before.columns,
            sidebar_before.rows,
        ),
        "no-direction native split must not change the sidebar: before \
         {sidebar_before:?}, after {sidebar_after:?}",
    );
}
