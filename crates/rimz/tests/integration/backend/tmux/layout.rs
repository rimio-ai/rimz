#![allow(clippy::print_stdout)]

use super::support::*;

#[test]
fn close_pane_on_exit_overrides_only_the_target_pane() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-pane-retention";
    ensure_rimz_session(&server, session, Some((120, 40)));
    server.tmux(&["set-option", "-g", "remain-on-exit", "on"]);
    server.tmux(&["set-option", "-w", "-t", session, "remain-on-exit", "on"]);
    let mut panes = Vec::new();
    for cue in ["pinned-exit", "retained-exit"] {
        let command = shlex::try_join([
            "tmux",
            "-S",
            server.socket.to_str().expect("socket path"),
            "wait-for",
            cue,
        ])
        .expect("quote exit cue");
        panes.push(server.stdout(&[
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            session,
            &command,
        ]));
    }
    let pinned = &panes[0];
    let retained = &panes[1];
    server.tmux(&["set-option", "-p", "-t", pinned, "remain-on-exit", "on"]);
    server
        .backend
        .close_pane_on_exit(&PaneId::from_parts(MuxName::Tmux, pinned))
        .unwrap();
    server.tmux(&["wait-for", "-S", "pinned-exit"]);
    server.tmux(&["wait-for", "-S", "retained-exit"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let remaining =
            server.stdout(&["list-panes", "-t", session, "-F", "#{pane_id} #{pane_dead}"]);
        if !remaining
            .lines()
            .any(|line| line.starts_with(&format!("{pinned} ")))
            && remaining
                .lines()
                .any(|line| line == format!("{retained} 1"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "pinned pane must close, sibling must stay dead: {remaining}"
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        server.stdout(&["show-options", "-gv", "remain-on-exit"]),
        "on"
    );
    assert_eq!(
        server.stdout(&["show-options", "-wv", "-t", session, "remain-on-exit"]),
        "on"
    );
}

#[test]
fn tab_injects_env_into_every_column_and_row() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-tab-env";
    ensure_rimz_session(&server, session, Some((200, 60)));
    let cwd = TempDir::new().unwrap();
    let (_stub_dir, stub) = delayed_sidebar_title_command_stub();
    let mut sidebar = sidebar_opts(server._tempdir.path(), session, stub, Some(200));
    sidebar.cwd = cwd.path().to_owned();
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
    server
        .backend
        .open_tab(&TabOptions {
            env: BTreeMap::from([("RIMZ_TEST_VAR".into(), "marker with spaces".into())]),
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
            let deadline = Instant::now() + Duration::from_secs(10);
            while !std::fs::read_to_string(&marker).is_ok_and(|text| !text.is_empty()) {
                assert!(Instant::now() < deadline, "missing {}", marker.display());
                thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(
                std::fs::read_to_string(marker).unwrap(),
                "marker with spaces"
            );
        }
    }
}

#[test]
fn companion_grid_preserves_processes_sidebar_and_focus() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-companion-grid";
    ensure_rimz_session(&server, session, Some((290, 90)));
    // Hook-born sidebars must be recognized from their spawn command, before
    // their process publishes the title used by the other chrome signals.
    let (_stub_dir, stub) = delayed_sidebar_title_command_stub();
    let sidebar = sidebar_opts(server._tempdir.path(), session, stub, Some(290));
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("sidebar");
    let original_focus = server.display(session, "#{window_id}:#{pane_id}");
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "children".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .expect("child tab");
    let tab = format!("{session}:children");
    let initial = server.wait_for_panes(&tab, 2);
    let sidebar = initial.iter().find(|pane| pane.left == 0).expect("sidebar");
    let anchor = initial
        .iter()
        .find(|pane| pane.left > 0)
        .expect("work pane");
    let anchor_id = PaneId::from_parts(MuxName::Tmux, &anchor.id);
    let child_focus = server.display(&tab, "#{pane_id}");
    let mut pids = BTreeMap::new();
    for count in 1_usize..=8 {
        let panes = server.wait_for_panes(&tab, count + 1);
        assert_eq!(panes.len(), count + 1);
        let live_sidebar = panes
            .iter()
            .find(|pane| pane.id == sidebar.id)
            .expect("sidebar survives");
        assert_eq!(
            (
                live_sidebar.left,
                live_sidebar.top,
                live_sidebar.width,
                live_sidebar.height
            ),
            (sidebar.left, sidebar.top, sidebar.width, sidebar.height),
        );
        let work = panes
            .iter()
            .filter(|pane| pane.id != sidebar.id)
            .collect::<Vec<_>>();
        let mut bands = BTreeMap::<u64, Vec<&PaneGeom>>::new();
        for pane in &work {
            assert!(pane.left > sidebar.width);
            bands.entry(pane.left).or_default().push(pane);
        }
        assert_eq!(bands.len(), count.min(2));
        assert!(bands.values().all(|band| band.len() <= count.div_ceil(2)));
        let spread = |sizes: Vec<u64>| {
            sizes.iter().max().expect("max size") - sizes.iter().min().expect("min size")
        };
        assert!(
            spread(bands.values().map(|band| band[0].width).collect()) <= 1,
            "equal column widths at {count} panes: {work:?}"
        );
        assert!(
            bands
                .values()
                .all(|band| spread(band.iter().map(|pane| pane.height).collect()) <= 1),
            "equal heights within each column at {count} panes: {work:?}"
        );
        for pane in &panes {
            let pid = server.display(&pane.id, "#{pane_pid}");
            if let Some(previous) = pids.insert(pane.id.clone(), pid.clone()) {
                assert_eq!(previous, pid, "process preserved in {}", pane.id);
            }
        }
        assert_eq!(pids.len(), panes.len(), "no pane was replaced");
        assert_eq!(
            server.display(session, "#{window_id}:#{pane_id}"),
            original_focus
        );
        assert_eq!(server.display(&tab, "#{pane_id}"), child_focus);
        if count == 6 {
            let held = work
                .iter()
                .find(|pane| pane.id != anchor.id)
                .expect("held child");
            server.tmux(&["set-option", "-p", "-t", &held.id, "remain-on-exit", "on"]);
            server.tmux(&["respawn-pane", "-k", "-t", &held.id, "true"]);
            let deadline = Instant::now() + Duration::from_secs(3);
            while server.display(&held.id, "#{pane_dead}") != "1" {
                assert!(Instant::now() < deadline, "child must become held");
                thread::sleep(Duration::from_millis(10));
            }
            pids.insert(held.id.clone(), server.display(&held.id, "#{pane_pid}"));
        }
        let outcome = server
            .backend
            .append_companion_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: session.to_owned(),
                    pane_id: anchor_id.clone(),
                },
                cwd: Some(std::env::temp_dir().display().to_string()),
                command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
                title: Some(format!("child-{}", count + 1)),
                close_on_exit: true,
                env: Default::default(),
                placement: SplitPlacement::Stacked,
                focus: true,
            })
            .expect("append companion");
        assert_eq!(
            outcome,
            if count == 8 {
                rimz::mux::CompanionPaneAppend::Full
            } else {
                rimz::mux::CompanionPaneAppend::Opened
            },
            "append at {count} work panes: {panes:?}; tmux: {}; anchor: {}; metadata: {:?}",
            server.stdout(&["-V"]),
            anchor.id,
            server.stdout(&[
                "list-panes",
                "-t",
                &anchor.id,
                "-F",
                "#{pane_id} #{pane_left} #{pane_width} #{pane_top} #{pane_height}|#{window_zoomed_flag}|#{pane_floating_flag}|#{==:#{pane_title},rimz-sidebar}|#{pane_current_command}|#{pane_start_command}",
            ])
        );
    }
    assert_eq!(
        server.wait_for_panes(&tab, 9).len(),
        9,
        "Full does not spawn"
    );
}

fn held_managed_pane(marker: &[&str]) -> rimz::mux::HostPane {
    let mut argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "exec sleep 120".to_owned(),
    ];
    argv.extend(marker.iter().map(|arg| (*arg).to_owned()));
    rimz::mux::HostPane {
        argv,
        cwd: std::env::temp_dir(),
    }
}

fn pane_has_marker(pane: &PaneRef, marker: &str) -> bool {
    [
        pane.spawn_command.as_deref(),
        pane.command.as_deref(),
        pane.title.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|command| command.contains(marker))
}

#[test]
fn open_background_view_births_columns_and_is_idempotent() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-bgview");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = sidebar_opts(server._tempdir.path(), "rimz-bgview", stub, Some(80));
    // Install the `after-new-window` sidebar hook the way `rimz start` does
    // before launching the host.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let working_window = server.display("rimz-bgview", "#{window_id}");
    let opts = rimz::mux::BackgroundViewOptions {
        view: rimz::mux::DaemonView {
            name: "rimzd".to_owned(),
            content: vec![sleep_host()],
            hosts: vec![sleep_host(), sleep_host()],
            loop_panel: sleep_host(),
        },
        sidebar: sidebar.clone(),
    };
    let first = server
        .backend
        .open_background_view(&opts)
        .expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        server
            .window_names("rimz-bgview")
            .iter()
            .any(|name| name == "rimzd"),
        "expected a rimzd window after launch, got {:?}",
        server.window_names("rimz-bgview"),
    );
    // Forced to the front: the daemon window leads the session.
    assert_eq!(
        server
            .window_names("rimz-bgview")
            .first()
            .map(String::as_str),
        Some("rimzd"),
        "daemon window must lead the session, got {:?}",
        server.window_names("rimz-bgview"),
    );
    assert_eq!(
        server.display("rimz-bgview", "#{window_id}"),
        working_window,
        "launch must leave focus on the pre-existing working window",
    );
    assert_ne!(
        server.display("rimz-bgview", "#{window_name}"),
        "rimzd",
        "launch must not focus the daemon window",
    );
    // Born `sidebar | content | runtime`: the hook-docked sidebar beside
    // content and the runtime column.
    let rc_panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-bgview".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("rimzd"))
        .count();
    assert_eq!(
        rc_panes, 5,
        "rimzd window should be born sidebar | content | runtime"
    );
    let panes = server.wait_for_panes("rimz-bgview:rimzd", 5);
    assert_eq!(panes.len(), 5, "expected five rimzd panes, got {panes:?}");
    let mut by_left: BTreeMap<u64, Vec<&PaneGeom>> = BTreeMap::new();
    for pane in &panes {
        by_left.entry(pane.left).or_default().push(pane);
    }
    assert_eq!(
        by_left.len(),
        3,
        "rimzd should have three columns: sidebar | content | runtime, got {panes:?}",
    );
    let right_column = by_left
        .iter()
        .next_back()
        .map(|(_, panes)| panes)
        .expect("right column");
    assert_eq!(
        right_column.len(),
        3,
        "runtime panes should share the right column, got {panes:?}",
    );
    let mut host_tops: Vec<u64> = right_column.iter().map(|pane| pane.top).collect();
    host_tops.sort_unstable();
    host_tops.dedup();
    assert_eq!(
        host_tops.len(),
        3,
        "runtime panes should be vertically stacked, got {panes:?}",
    );
    let runtime_heights = right_column
        .iter()
        .map(|pane| pane.height)
        .collect::<Vec<_>>();
    assert!(
        runtime_heights.iter().max().expect("runtime height")
            - runtime_heights.iter().min().expect("runtime height")
            <= 1,
        "runtime pane heights should be equal within rounding, got {panes:?}",
    );
    let second = server
        .backend
        .open_background_view(&opts)
        .expect("second launch");
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
    let stats = rimz::mux::BackgroundViewOptions {
        view: rimz::mux::DaemonView {
            name: "rimzd-stats".to_owned(),
            content: vec![sleep_host()],
            hosts: Vec::new(),
            loop_panel: sleep_host(),
        },
        sidebar: sidebar.clone(),
    };
    assert_eq!(
        server
            .backend
            .open_background_view(&stats)
            .expect("stats launch"),
        rimz::mux::BackgroundViewLaunch::Launched,
    );
    let panes = server.wait_for_panes("rimz-bgview:rimzd-stats", 3);
    assert_eq!(
        panes.len(),
        3,
        "rimzd-stats window should be born sidebar | content | loop panel"
    );
    let stack = rimz::mux::BackgroundViewOptions {
        view: rimz::mux::DaemonView {
            name: "rimzd-stack".to_owned(),
            content: vec![sleep_host(), sleep_host()],
            hosts: Vec::new(),
            loop_panel: sleep_host(),
        },
        sidebar,
    };
    assert_eq!(
        server
            .backend
            .open_background_view(&stack)
            .expect("stack launch"),
        rimz::mux::BackgroundViewLaunch::Launched,
    );
    let panes = server.wait_for_panes("rimz-bgview:rimzd-stack", 4);
    assert_eq!(
        panes.len(),
        4,
        "expected four rimzd-stack panes, got {panes:?}"
    );
    let mut by_left: BTreeMap<u64, Vec<&PaneGeom>> = BTreeMap::new();
    for pane in &panes {
        by_left.entry(pane.left).or_default().push(pane);
    }
    assert_eq!(
        by_left.len(),
        3,
        "rimzd should have three columns: sidebar | content | loop panel, got {panes:?}",
    );
    let content_column = by_left
        .iter()
        .nth(1)
        .map(|(_, panes)| panes)
        .expect("content column");
    assert_eq!(
        content_column.len(),
        2,
        "content panes should share the middle column, got {panes:?}",
    );
    let mut content_tops: Vec<u64> = content_column.iter().map(|pane| pane.top).collect();
    content_tops.sort_unstable();
    content_tops.dedup();
    assert_eq!(
        content_tops.len(),
        2,
        "content panes should be vertically stacked, got {panes:?}",
    );
    let content_heights = content_column
        .iter()
        .map(|pane| pane.height)
        .collect::<Vec<_>>();
    assert!(
        content_heights.iter().max().expect("content height")
            - content_heights.iter().min().expect("content height")
            <= 1,
        "content pane heights should be equal within rounding, got {panes:?}",
    );
}

#[test]
fn repair_daemon_view_recreates_missing_runtime_panes_in_one_column() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-bg-repair");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = sidebar_opts(server._tempdir.path(), "rimz-bg-repair", stub, Some(80));
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let view = rimz::mux::DaemonView {
        name: rimz::pane::VIEW_NAME.to_owned(),
        content: vec![held_managed_pane(&[
            "rimz", "daemon", "content", "--slot", "0",
        ])],
        hosts: vec![held_managed_pane(&["rimz", "codex", "app-server", "serve"])],
        loop_panel: held_managed_pane(&["rimz", "loop", "watch", "--hold"]),
    };
    server
        .backend
        .open_background_view(&rimz::mux::BackgroundViewOptions {
            view: view.clone(),
            sidebar: sidebar.clone(),
        })
        .expect("open daemon view");
    let listing = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-bg-repair".to_owned()),
            consistency: PaneReadConsistency::PreferAuthoritative,
            ..Default::default()
        })
        .expect("list panes");
    let panel = rimz::daemon_view::find_loop_panel(&listing.panes)
        .expect("loop panel")
        .pane_id
        .clone();
    let broker = listing
        .panes
        .iter()
        .find(|pane| pane_has_marker(pane, "app-server"))
        .expect("Codex broker")
        .pane_id
        .clone();
    let geometry = server
        .backend
        .view_geometry("rimz-bg-repair", &broker, Duration::from_secs(3))
        .expect("view geometry");
    assert!(
        geometry.is_some(),
        "a fresh daemon window has tiled geometry"
    );
    let geometry = geometry.unwrap();
    assert_eq!(geometry.gap, 1);
    let native = server.wait_for_panes("rimz-bg-repair:rimzd", 4);
    assert_eq!(geometry.panes.len(), native.len());
    for rect in &geometry.panes {
        let pane = native
            .iter()
            .find(|pane| pane.id == rect.pane_id.raw())
            .unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.cols, rect.rows),
            (pane.left, pane.top, pane.width, pane.height),
        );
    }
    assert_eq!(
        rimz::daemon_view::inspect_daemon_shape(
            &server.backend,
            "rimz-bg-repair",
            &sidebar.workspace_id
        )
        .unwrap(),
        Some(rimz::daemon_view::DaemonShape::Healthy),
    );
    server.tmux(&["resize-pane", "-Z", "-t", broker.raw()]);
    assert!(
        server
            .backend
            .view_geometry("rimz-bg-repair", &broker, Duration::from_secs(3))
            .unwrap()
            .is_none()
    );
    server.tmux(&["resize-pane", "-Z", "-t", broker.raw()]);
    server.tmux(&["set-option", "-p", "-t", broker.raw(), "@rimz_stack", "1"]);
    assert!(
        server
            .backend
            .view_geometry("rimz-bg-repair", &broker, Duration::from_secs(3))
            .unwrap()
            .is_none()
    );
    server.tmux(&["set-option", "-pu", "-t", broker.raw(), "@rimz_stack"]);
    server.tmux(&["kill-pane", "-t", panel.raw()]);
    server.tmux(&["kill-pane", "-t", broker.raw()]);

    rimz::daemon_view::repair_daemon_view(
        &server.backend,
        "rimz-bg-repair",
        &sidebar.workspace_id,
        &view,
    );

    let panes = server.wait_for_panes("rimz-bg-repair:rimzd", 4);
    assert_eq!(
        panes.len(),
        4,
        "repair should restore both runtime panes: {panes:?}"
    );
    let listing = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-bg-repair".to_owned()),
            consistency: PaneReadConsistency::PreferAuthoritative,
            ..Default::default()
        })
        .expect("list repaired panes");
    let panel = rimz::daemon_view::find_loop_panel(&listing.panes).expect("repaired loop panel");
    let broker = listing
        .panes
        .iter()
        .find(|pane| pane_has_marker(pane, "app-server"))
        .expect("repaired Codex broker");
    let content = listing
        .panes
        .iter()
        .find(|pane| pane_has_marker(pane, "daemon content --slot 0"))
        .expect("content pane");
    let panel_left = panes
        .iter()
        .find(|pane| pane.id == panel.pane_id.raw())
        .expect("loop panel geometry")
        .left;
    let broker_left = panes
        .iter()
        .find(|pane| pane.id == broker.pane_id.raw())
        .expect("broker geometry")
        .left;
    let content_left = panes
        .iter()
        .find(|pane| pane.id == content.pane_id.raw())
        .expect("content geometry")
        .left;
    assert_eq!(
        broker_left, panel_left,
        "repaired runtime panes must share one column: {panes:?}"
    );
    assert!(
        content_left < broker_left,
        "runtime column must stay to the right of content: {panes:?}"
    );
}

#[test]
fn daemon_shape_repairs_spanning_loop_without_restarting_the_app_server() {
    use rimz::daemon_view::{DaemonShape, RepairOutcome, inspect_daemon_shape, repair_daemon_view};
    use rimz::mux::SplitDirection;

    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-shape-repair";
    ensure_rimz_session(&server, session, Some((256, 81)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = sidebar_opts(server._tempdir.path(), session, stub, Some(256));
    let view = rimz::mux::DaemonView {
        name: rimz::pane::VIEW_NAME.to_owned(),
        content: vec![held_managed_pane(&[
            "rimz", "daemon", "content", "--slot", "0",
        ])],
        hosts: vec![held_managed_pane(&["rimz", "codex", "app-server", "serve"])],
        loop_panel: held_managed_pane(&["rimz", "loop", "watch", "--hold"]),
    };
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: view.name.clone(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: view.content[0].argv.clone(),
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar.clone(),
        })
        .unwrap();
    let tab = format!("{session}:rimzd");
    let initial = server.wait_for_panes(&tab, 2);
    let content = initial
        .iter()
        .max_by_key(|pane| pane.left)
        .unwrap()
        .id
        .clone();
    let split = |pane: &rimz::mux::HostPane, direction| {
        server
            .backend
            .split_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: session.to_owned(),
                    pane_id: PaneId::from_parts(MuxName::Tmux, &content),
                },
                command: Some(pane.argv.clone()),
                title: Some(pane.argv.join(" ")),
                close_on_exit: true,
                placement: SplitPlacement::Directional(direction),
                focus: false,
                ..Default::default()
            })
            .unwrap()
            .unwrap()
    };
    let panel = split(&view.loop_panel, SplitDirection::Down);
    let broker = split(&view.hosts[0], SplitDirection::Right);
    let broker_pid = server.display(broker.raw(), "#{pane_pid}");
    let content_pid = server.display(&content, "#{pane_pid}");
    let shape = inspect_daemon_shape(&server.backend, session, &sidebar.workspace_id).unwrap();
    assert!(
        matches!(shape, Some(DaemonShape::Repairable { .. })),
        "{shape:?}"
    );
    assert_eq!(
        repair_daemon_view(&server.backend, session, &sidebar.workspace_id, &view),
        RepairOutcome::Converged
    );
    let after = server.wait_for_panes(&tab, 4);
    assert_eq!(server.display(broker.raw(), "#{pane_pid}"), broker_pid);
    assert_eq!(server.display(&content, "#{pane_pid}"), content_pid);
    assert!(
        !after.iter().any(|pane| pane.id == panel.raw()),
        "only loop is restarted"
    );
    let listing = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .unwrap();
    let loop_id = &rimz::daemon_view::find_loop_panel(&listing.panes)
        .unwrap()
        .pane_id;
    let panel = after.iter().find(|pane| pane.id == loop_id.raw()).unwrap();
    let broker = after.iter().find(|pane| pane.id == broker.raw()).unwrap();
    let content = after.iter().find(|pane| pane.id == content).unwrap();
    let bottom = after
        .iter()
        .map(|pane| pane.top + pane.height)
        .max()
        .unwrap();
    assert_eq!((content.top, content.height), (0, bottom));
    assert_eq!((broker.left, broker.width), (panel.left, panel.width));
    assert_eq!(broker.top, 0);
    assert_eq!(broker.height + 1, panel.top);
    assert_eq!(panel.top + panel.height, bottom);
    assert!(panel.left > content.left + content.width);
    assert_eq!(
        inspect_daemon_shape(&server.backend, session, &sidebar.workspace_id).unwrap(),
        Some(DaemonShape::Healthy)
    );
}

#[test]
fn open_tab_builds_multi_column_layout() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&session_opts(
            "rimz-tab",
            WorkspaceId::from_project_root(cwd.path()),
            cwd.path(),
            cwd.path(),
            Some((300, 50)),
        ))
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts(server._tempdir.path(), "rimz-tab", stub, Some(300))
    };
    // Installs the `after-new-window` hook so the new tab is born with a sidebar.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };
    let sidebar_width = u64::from(
        sidebar
            .target
            .cols(sidebar.detected_view_size.map(|(cols, _)| cols))
            .get(),
    );
    let want_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    // Each shape is born as full-height columns of even rows: the reported
    // two-by-two team layout, a ragged pair, and a three-column mix.
    for (title, rows_per_column) in [
        ("work", &[2_usize, 2][..]),
        ("ragged", &[2, 1][..]),
        ("mixed", &[1, 2, 3][..]),
    ] {
        server
            .backend
            .open_tab(&TabOptions {
                env: Default::default(),
                title: title.to_owned(),
                panes: LayoutPanes {
                    columns: rows_per_column
                        .iter()
                        .map(|rows| tiled_column((0..*rows).map(|_| work_pane()).collect()))
                        .collect(),
                    focused_pane: rows_per_column.iter().sum::<usize>() - 1,
                },
                focus: true,
                dock_sidebar: true,
                after: None,
                sidebar: sidebar.clone(),
            })
            .expect("open_tab");
        let target = format!("rimz-tab:{title}");
        let panes = server.wait_for_panes(&target, rows_per_column.iter().sum::<usize>() + 1);
        let window_height: u64 = server
            .display(&target, "#{window_height}")
            .parse()
            .expect("window height");
        assert_grid(&panes, window_height, sidebar_width, rows_per_column);
        let last_pane = panes
            .iter()
            .max_by_key(|pane| (pane.left, pane.top))
            .expect("a work pane");
        assert_eq!(
            server.display(&target, "#{pane_left},#{pane_top}"),
            format!("{},{}", last_pane.left, last_pane.top),
            "focused_pane selects the last layout pane: {panes:?}",
        );
        // Every work pane runs in the requested cwd.
        for pane in panes.iter().filter(|pane| pane.left > 0) {
            assert_eq!(
                Path::new(&pane.path).canonicalize().ok().as_deref(),
                Some(want_cwd.as_path()),
                "each work pane runs in the tab cwd: {pane:?}",
            );
        }
        // `focus: true` made the new tab the session's current window.
        assert_eq!(
            server.display("rimz-tab", "#{window_name}"),
            title,
            "focus: true should select the new window",
        );
    }
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "solo".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .expect("open solo tab");
    let panes = server.wait_for_panes("rimz-tab:solo", 2);
    assert_eq!(
        panes.len(),
        2,
        "a single-pane layout is born `sidebar | work`: {panes:?}",
    );
    assert_eq!(
        panes.iter().filter(|p| p.left == 0).count(),
        1,
        "the sidebar docks at the left edge: {panes:?}",
    );
    let work = panes
        .iter()
        .find(|p| p.left > 0)
        .expect("a work pane to the right of the sidebar");
    assert_eq!(
        Path::new(&work.path).canonicalize().ok().as_deref(),
        Some(want_cwd.as_path()),
        "the work pane runs in the tab cwd: {work:?}",
    );
    assert_eq!(
        server.display("rimz-tab", "#{window_name}"),
        "mixed",
        "focus: false should leave the session on its previous window",
    );
}

#[test]
fn stacked_splits_collapse_the_column() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&session_opts(
            "rimz-stacked",
            WorkspaceId::from_project_root(cwd.path()),
            cwd.path(),
            cwd.path(),
            Some((120, 50)),
        ))
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts(server._tempdir.path(), "rimz-stacked", stub, Some(120))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "stacked".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .expect("open_tab");

    let initial = server.wait_for_panes("rimz-stacked:stacked", 2);
    let sidebar_width = initial
        .iter()
        .find(|pane| pane.left == 0)
        .expect("sidebar pane")
        .width;
    let work_width = initial
        .iter()
        .find(|pane| pane.left > 0)
        .expect("work pane")
        .width;
    let mut target = PaneId::from_parts(
        MuxName::Tmux,
        initial
            .iter()
            .find(|pane| pane.left > 0)
            .expect("work pane")
            .id
            .clone(),
    );
    let mut known = initial
        .iter()
        .map(|pane| pane.id.clone())
        .collect::<std::collections::BTreeSet<_>>();

    server.tmux(&["select-pane", "-t", &initial[0].id]);

    for _ in 0..2 {
        server
            .backend
            .split_pane(SplitPaneOptions {
                target: SplitTarget::Pane(target),
                cwd: Some(cwd.path().display().to_string()),
                command: Some(vec!["sleep".to_owned(), "600".to_owned()]),
                title: None,
                close_on_exit: true,
                env: Default::default(),
                placement: SplitPlacement::Stacked,
                focus: false,
            })
            .expect("stacked split");
        let panes = server.wait_for_panes("rimz-stacked:stacked", known.len() + 1);
        let new_id = panes
            .iter()
            .map(|pane| pane.id.clone())
            .find(|id| !known.contains(id))
            .expect("new split pane");
        known.insert(new_id.clone());
        target = PaneId::from_parts(MuxName::Tmux, new_id);
    }

    let panes = server.wait_for_panes("rimz-stacked:stacked", 4);
    assert_eq!(
        panes
            .iter()
            .find(|pane| pane.left == 0)
            .expect("sidebar pane")
            .width,
        sidebar_width,
        "stacked splits must not resize the sidebar: {panes:?}",
    );
    let work = panes
        .iter()
        .filter(|pane| pane.left > 0)
        .collect::<Vec<_>>();
    assert_eq!(work.len(), 3, "expected three work panes: {panes:?}");
    assert!(
        work.iter().all(|pane| pane.width == work_width),
        "stacked splits must preserve the target column width: {panes:?}",
    );
    assert_stack(
        &server,
        &work.iter().map(|pane| pane.id.clone()).collect::<Vec<_>>(),
        target.raw(),
    );
}

fn stack_split(server: &TmuxServer, target: &str, placement: SplitPlacement) -> PaneId {
    server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Pane(PaneId::from_parts(MuxName::Tmux, target)),
            cwd: Some(std::env::temp_dir().display().to_string()),
            command: Some(vec!["sleep".into(), "600".into()]),
            title: Some("stack member".into()),
            close_on_exit: true,
            env: Default::default(),
            placement,
            focus: false,
        })
        .unwrap()
        .unwrap()
}

fn stack_fixture(server: &TmuxServer) -> (String, Vec<String>) {
    ensure_rimz_session(server, "stacks", Some((160, 60)));
    let caller = server.display("stacks", "#{pane_id}");
    let first = stack_split(
        server,
        &caller,
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    let second = stack_split(server, first.raw(), SplitPlacement::Stacked);
    let third = stack_split(server, second.raw(), SplitPlacement::Stacked);
    (
        caller,
        vec![first.raw().into(), second.raw().into(), third.raw().into()],
    )
}

fn assert_stack(server: &TmuxServer, members: &[String], open: &str) {
    let marks = members
        .iter()
        .map(|pane| server.display(pane, "#{@rimz_stack}"))
        .collect::<Vec<_>>();
    assert!(!marks[0].is_empty(), "members must be marked: {marks:?}");
    assert!(
        marks.iter().all(|mark| *mark == marks[0]),
        "one stack id: {marks:?}"
    );
    for pane in members {
        assert_eq!(
            server.display(pane, "#{@rimz_stack_open}"),
            if pane == open { "1" } else { "" },
            "open flag on {pane}; {}",
            server.stdout(&[
                "list-panes",
                "-t",
                pane,
                "-F",
                "#{pane_id}|#{@rimz_stack}|#{@rimz_stack_open}|#{pane_height}|#{pane_active}"
            ])
        );
        let height: u64 = server.display(pane, "#{pane_height}").parse().unwrap();
        assert!(
            if pane == open {
                height > 1
            } else {
                height == 1
            },
            "{pane} height {height}, open {open}"
        );
    }
}

#[test]
fn stack_focus_resize_close_and_zoom_restore_the_open_member() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, mut members) = stack_fixture(&server);
    server.tmux(&["select-pane", "-t", &members[0]]);
    assert_stack(&server, &members, &members[0]);
    server.tmux(&["select-pane", "-D"]);
    assert_stack(&server, &members, &members[1]);
    server.tmux(&["select-pane", "-U"]);
    assert_stack(&server, &members, &members[0]);
    server.tmux(&["select-pane", "-t", &caller]);
    server.tmux(&["resize-window", "-t", &caller, "-y", "80"]);
    assert_stack(&server, &members, &members[0]);
    server.tmux(&["kill-pane", "-t", &members.pop().unwrap()]);
    assert_stack(&server, &members, &members[0]);
    server.tmux(&["select-pane", "-t", &members[0]]);
    server.tmux(&["resize-pane", "-Z", "-t", &members[0]]);
    server.tmux(&["select-pane", "-Z", "-t", &members[1]]);
    server.tmux(&["resize-window", "-t", &members[1], "-y", "90"]);
    assert_eq!(server.display(&members[1], "#{window_zoomed_flag}"), "1");
    server.tmux(&["resize-pane", "-Z", "-t", &members[1]]);
    assert_stack(&server, &members, &members[1]);
    server.tmux(&["kill-pane", "-t", &members.pop().unwrap()]);
    assert_stack(&server, &members, &members[0]);
}

fn pane_shapes(
    server: &TmuxServer,
    anchor: &str,
    count: usize,
) -> BTreeMap<String, (u64, u64, u64, u64)> {
    server
        .wait_for_panes(anchor, count)
        .into_iter()
        .map(|pane| (pane.id, (pane.left, pane.top, pane.width, pane.height)))
        .collect()
}

fn assert_directional_beside(
    server: &TmuxServer,
    members: &[String],
    foreign: &[String],
    direction: rimz::mux::SplitDirection,
    focus: bool,
) {
    let count = members.len() + foreign.len();
    let before = pane_shapes(server, &members[0], count);
    let window = server.display(&members[0], "#{window_id}");
    let active = server.display(&window, "#{pane_id}");
    let open = members
        .iter()
        .find(|id| server.display(id, "#{@rimz_stack_open}") == "1")
        .unwrap();
    let left = before[&members[0]].0;
    let width = before[&members[0]].2;
    let top = members.iter().map(|id| before[id].1).min().unwrap();
    let bottom = members
        .iter()
        .map(|id| before[id].1 + before[id].3)
        .max()
        .unwrap();
    let created = server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Pane(PaneId::from_parts(MuxName::Tmux, &members[0])),
            placement: SplitPlacement::Directional(direction),
            focus,
            command: Some(vec!["sleep".into(), "600".into()]),
            ..Default::default()
        })
        .unwrap()
        .unwrap();
    assert_eq!(
        server.display(created.raw(), "#{@rimz_stack}"),
        "",
        "a beside pane is not a stack member"
    );
    assert_stack(server, members, open);
    let after = pane_shapes(server, &members[0], count + 1);
    let new = after[created.raw()];
    match direction {
        rimz::mux::SplitDirection::Right => {
            assert_eq!((new.1, new.3), (top, bottom - top));
            assert!(new.2.abs_diff(width / 2) <= 1, "{before:?}; {after:?}");
            for id in members {
                let member = after[id];
                assert!(member.0 + member.2 < new.0, "{after:?}");
                assert_eq!((member.1, member.3), (before[id].1, before[id].3));
            }
        }
        rimz::mux::SplitDirection::Down => {
            assert_eq!((new.0, new.2), (left, width));
            assert!(
                new.3.abs_diff((bottom - top) / 2) <= 1,
                "{before:?}; {after:?}"
            );
            for id in members {
                assert!(after[id].1 + after[id].3 < new.1, "{after:?}");
            }
        }
    }
    for id in foreign {
        assert_eq!(after[id], before[id], "foreign pane {id}");
    }
    assert_eq!(
        server.display(&window, "#{pane_id}"),
        if focus { created.raw() } else { &active }
    );
    let mut ordered = members.to_vec();
    ordered.sort_by_key(|id| after[id].1);
    assert_eq!(&ordered, members);
    server.tmux(&["select-pane", "-t", &members[1]]);
    assert_stack(server, members, &members[1]);
    let restored = pane_shapes(server, &members[0], count + 1);
    assert_eq!(
        restored[created.raw()],
        new,
        "stack focus must not resize its neighbour"
    );
    for id in foreign {
        assert_eq!(restored[id], before[id], "foreign pane {id} after focus");
    }
}

#[test]
fn directional_right_split_goes_beside_the_whole_stack() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    server.tmux(&["new-window", "-t", "stacks", "-n", "other", "sleep 600"]);
    assert_directional_beside(
        &server,
        &members,
        &[caller],
        rimz::mux::SplitDirection::Right,
        false,
    );
    assert_eq!(server.display("stacks", "#{window_name}"), "other");
}

#[test]
fn directional_down_split_goes_beside_the_whole_stack() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    assert_directional_beside(
        &server,
        &members,
        &[caller],
        rimz::mux::SplitDirection::Down,
        true,
    );
}

#[test]
fn directional_right_split_needs_no_open_member() {
    directional_split_without_an_open_member(rimz::mux::SplitDirection::Right);
}

#[test]
fn directional_down_split_needs_no_open_member() {
    directional_split_without_an_open_member(rimz::mux::SplitDirection::Down);
}

fn directional_split_without_an_open_member(direction: rimz::mux::SplitDirection) {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, mut members) = stack_fixture(&server);
    server.tmux(&["kill-pane", "-t", &members.pop().unwrap()]);
    let before = pane_shapes(&server, &caller, 3);
    let mark = server.display(&members[0], "#{@rimz_stack}");
    for id in &members {
        assert_eq!(server.display(id, "#{@rimz_stack_open}"), "");
    }
    let new = stack_split(&server, &members[0], SplitPlacement::Directional(direction));
    assert_eq!(
        server.display(new.raw(), "#{@rimz_stack}"),
        "",
        "{direction:?} must go beside a stack with no open flag"
    );
    let after = pane_shapes(&server, &caller, 4);
    assert_eq!(after[&caller], before[&caller]);
    let neighbour = after[new.raw()];
    for id in &members {
        assert_eq!(server.display(id, "#{@rimz_stack}"), mark);
        assert_eq!(server.display(id, "#{@rimz_stack_open}"), "");
        match direction {
            rimz::mux::SplitDirection::Right => {
                assert!(after[id].0 + after[id].2 < neighbour.0, "{after:?}");
                assert_eq!((after[id].1, after[id].3), (before[id].1, before[id].3));
            }
            rimz::mux::SplitDirection::Down => {
                assert!(after[id].1 + after[id].3 < neighbour.1, "{after:?}");
                assert_eq!((after[id].0, after[id].2), (before[id].0, before[id].2));
            }
        }
    }
    match direction {
        rimz::mux::SplitDirection::Right => {
            assert_eq!(
                (neighbour.1, neighbour.3),
                (before[&caller].1, before[&caller].3)
            )
        }
        rimz::mux::SplitDirection::Down => assert_eq!(
            (neighbour.0, neighbour.2),
            (before[&members[0]].0, before[&members[0]].2)
        ),
    }
}

#[test]
fn directional_splits_isolate_a_stack_from_foreign_rows() {
    require_tmux!();
    for direction in [
        rimz::mux::SplitDirection::Right,
        rimz::mux::SplitDirection::Down,
    ] {
        let server = TmuxServer::new();
        ensure_rimz_session(&server, "stacks", Some((160, 70)));
        let above = server.display("stacks", "#{pane_id}");
        let first = stack_split(
            &server,
            &above,
            SplitPlacement::Directional(rimz::mux::SplitDirection::Down),
        );
        let second = stack_split(&server, first.raw(), SplitPlacement::Stacked);
        let third = stack_split(&server, second.raw(), SplitPlacement::Stacked);
        let members: [String; 3] = [first.raw().into(), second.raw().into(), third.raw().into()];
        server.tmux(&["select-pane", "-t", &members[0]]);
        assert_directional_beside(&server, &members, &[above], direction, false);
    }
}

#[test]
fn directional_split_dissolves_a_lone_stack_even_after_refocus() {
    require_tmux!();
    for direction in [
        rimz::mux::SplitDirection::Down,
        rimz::mux::SplitDirection::Right,
    ] {
        let server = TmuxServer::new();
        let (caller, members) = stack_fixture(&server);
        let anchor = stack_split(
            &server,
            &caller,
            SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
        );
        let second = stack_split(&server, anchor.raw(), SplitPlacement::Stacked);
        server.tmux(&["select-pane", "-t", &members[0]]);
        for id in &members[1..] {
            server.tmux(&["kill-pane", "-t", id]);
        }
        server.tmux(&["select-pane", "-t", &caller]);
        let before = pane_shapes(&server, &caller, 4)[&members[0]];
        let new = stack_split(&server, &members[0], SplitPlacement::Directional(direction));
        let initial = pane_shapes(&server, &caller, 5)[new.raw()];
        match direction {
            rimz::mux::SplitDirection::Down => assert!(
                initial.3.abs_diff(before.3 / 2) <= 1,
                "{direction:?} newcomer crushed at birth: before {before:?}, new {initial:?}"
            ),
            rimz::mux::SplitDirection::Right => assert!(
                initial.2.abs_diff(before.2 / 2) <= 1,
                "{direction:?} newcomer crushed at birth: before {before:?}, new {initial:?}"
            ),
        }
        server.tmux(&["select-pane", "-t", &members[0]]);
        // Exercise restore after refocus without changing the requested geometry.
        let layout = server.display(&caller, "#{window_layout}");
        server.tmux(&["select-layout", "-t", &caller, &layout]);
        assert_eq!(
            pane_shapes(&server, &caller, 5)[new.raw()],
            initial,
            "{direction:?} newcomer resized by restore after refocusing the survivor"
        );
        server.tmux(&["select-pane", "-t", anchor.raw()]);
        assert_eq!(
            pane_shapes(&server, &caller, 5)[new.raw()],
            initial,
            "{direction:?} newcomer resized after refocusing the other stack"
        );
        assert_stack(
            &server,
            &[anchor.raw().into(), second.raw().into()],
            anchor.raw(),
        );
        for pane in [members[0].as_str(), new.raw()] {
            assert_eq!(server.display(pane, "#{@rimz_stack}"), "");
            assert_eq!(server.display(pane, "#{@rimz_stack_open}"), "");
        }
    }
}

#[test]
fn directional_split_of_a_broken_stack_falls_back_to_join() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    server.tmux(&["swap-pane", "-s", &members[1], "-t", &caller]);
    let mark = server.display(&members[0], "#{@rimz_stack}");
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(server.display(new.raw(), "#{@rimz_stack}"), mark);
}

#[test]
fn directional_stack_split_refreshes_members_after_a_member_exits() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    server.tmux(&[
        "set-hook",
        "-g",
        "after-split-window",
        &format!(
            "set-hook -gu -t {caller} after-split-window ; kill-pane -t {}",
            members[1]
        ),
    ]);
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert!(!pane_shapes(&server, &caller, 4).contains_key(&members[1]));
    assert_joined_after_stack_refusal(
        &server,
        &[members[0].clone(), members[2].clone(), new.raw().into()],
        new.raw(),
    );
}

#[test]
fn directional_stack_split_refuses_changed_marks_without_changed_leaves() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    server.tmux(&[
        "set-hook",
        "-g",
        "after-split-window",
        &format!(
            "set-hook -gu -t {caller} after-split-window ; set-option -pu -t {} @rimz_stack",
            members[1]
        ),
    ]);
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(server.display(&members[1], "#{@rimz_stack}"), "");
    assert_joined_after_stack_refusal(
        &server,
        &[members[0].clone(), members[2].clone(), new.raw().into()],
        new.raw(),
    );
}

#[test]
fn directional_stack_split_refreshes_flags_on_a_concurrent_joiner() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, mut members) = stack_fixture(&server);
    let mark = server.display(&members[0], "#{@rimz_stack}");
    let incoming = server.stdout(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "stacks",
        "sleep 600",
    ]);
    server.tmux(&["set-hook", "-g", "after-split-window", &format!(
        "set-hook -gu -t {caller} after-split-window ; join-pane -v -d -s {incoming} -t {} ; set-option -p -t {incoming} @rimz_stack {mark} ; set-option -p -t {incoming} @rimz_stack_open 1", members[2]
    )]);
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(pane_shapes(&server, &caller, 6).len(), 6);
    members.extend([incoming, new.raw().into()]);
    assert_joined_after_stack_refusal(&server, &members, new.raw());
}

fn assert_joined_after_stack_refusal(server: &TmuxServer, members: &[String], open: &str) {
    let mark = server.display(&members[0], "#{@rimz_stack}");
    assert!(!mark.is_empty());
    for pane in members {
        assert_eq!(
            server.display(pane, "#{@rimz_stack}"),
            mark,
            "member {pane}"
        );
        assert_eq!(
            server.display(pane, "#{@rimz_stack_open}"),
            if pane == open { "1" } else { "" },
            "open flag on {pane}"
        );
    }
    assert!(
        server
            .display(open, "#{pane_height}")
            .parse::<u64>()
            .unwrap()
            > 1
    );
    assert_eq!(server.display(open, "#{@rimz_stack_layout_guard}"), "");
}

#[test]
fn directional_stack_split_preserves_an_after_split_resize() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, mut members) = stack_fixture(&server);
    server.tmux(&[
        "set-hook",
        "-g",
        "after-split-window",
        &format!("set-hook -gu -t {caller} after-split-window ; resize-pane -t {caller} -x 110"),
    ]);
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(
        server.display(&caller, "#{pane_width}"),
        "110",
        "the rewrite must not undo the after-split hook's resize"
    );
    members.push(new.raw().into());
    assert_joined_after_stack_refusal(&server, &members, new.raw());
}

#[test]
fn directional_stack_split_guards_the_layout_read_before_apply() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    // Change geometry after the applying batch records the read, before its check.
    server.tmux(&["set-hook", "-g", "after-set-option", &format!(
        "if-shell -F -t {caller} '#{{@rimz_stack_layout_guard}}' {{ set-hook -gu -t {caller} after-set-option ; resize-pane -t {caller} -x 90 }}"
    )]);
    let new = stack_split(
        &server,
        &members[0],
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(
        server.display(&caller, "#{pane_width}"),
        "90",
        "a stale rewrite must not undo the intervening resize"
    );
    assert_eq!(
        server.display(new.raw(), "#{@rimz_stack}"),
        server.display(&members[0], "#{@rimz_stack}")
    );
    assert_eq!(server.display(new.raw(), "#{@rimz_stack_layout_guard}"), "");
}

#[test]
fn focus_changes_preserve_two_independent_stacks() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, first) = stack_fixture(&server);
    let anchor = stack_split(
        &server,
        &caller,
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    let middle = stack_split(&server, anchor.raw(), SplitPlacement::Stacked);
    let bottom = stack_split(&server, middle.raw(), SplitPlacement::Stacked);
    let stacks = [
        first,
        vec![
            anchor.raw().into(),
            middle.raw().into(),
            bottom.raw().into(),
        ],
    ];
    assert_ne!(
        server.display(&stacks[0][0], "#{@rimz_stack}"),
        server.display(&stacks[1][0], "#{@rimz_stack}")
    );
    let shape = |members: &[String]| {
        members
            .iter()
            .map(|pane| {
                server.display(
                    pane,
                    "#{pane_id}|#{@rimz_stack}|#{@rimz_stack_open}|#{pane_left}|#{pane_top}|#{pane_width}|#{pane_height}",
                )
            })
            .collect::<Vec<_>>()
    };
    let mut open = [2, 2];
    for (index, members) in stacks.iter().enumerate() {
        assert_stack(&server, members, &members[open[index]]);
    }
    for index in [0, 1, 0, 1] {
        let other = 1 - index;
        let unchanged = shape(&stacks[other]);
        for (args, expanded) in [
            (vec!["select-pane", "-t", stacks[index][0].as_str()], 0),
            (vec!["select-pane", "-D"], 1),
            (vec!["select-pane", "-D"], 2),
            (vec!["select-pane", "-U"], 1),
        ] {
            server.tmux(&args);
            open[index] = expanded;
            for (stack, members) in stacks.iter().enumerate() {
                assert_stack(&server, members, &members[open[stack]]);
            }
            assert_eq!(shape(&stacks[other]), unchanged);
        }
    }
}

#[test]
fn collapsed_member_can_split_and_restart_without_losing_membership() {
    require_tmux!();
    let server = TmuxServer::new();
    let (_, mut members) = stack_fixture(&server);
    server.tmux(&[
        "set-hook",
        "-g",
        "after-split-window",
        "set-option -p -F @stack_birth_height '#{pane_height}'",
    ]);
    let new = stack_split(&server, &members[0], SplitPlacement::Stacked);
    assert!(
        server
            .display(new.raw(), "#{@stack_birth_height}")
            .parse::<u64>()
            .unwrap()
            > 1,
        "new pane must not be collapsed before its open decision"
    );
    members.push(new.raw().into());
    assert_stack(&server, &members, new.raw());
    server.tmux(&["select-pane", "-t", &members[1]]);
    let replacement = stack_split(
        &server,
        &members[0],
        SplitPlacement::Rejoin(rimz::mux::SplitDirection::Right),
    );
    members.push(replacement.raw().into());
    let old = members.remove(0);
    server.tmux(&["kill-pane", "-t", &old]);
    assert_stack(&server, &members, &members[0]);
}

#[test]
fn stacked_splits_preserve_panes_above_the_caller() {
    require_tmux!();
    let server = TmuxServer::new();
    ensure_rimz_session(&server, "stacks", Some((160, 60)));
    let above = server.display("stacks", "#{pane_id}");
    server.tmux(&[
        "set-window-option",
        "-t",
        &above,
        "pane-border-status",
        "top",
    ]);
    let caller = stack_split(
        &server,
        &above,
        SplitPlacement::Directional(rimz::mux::SplitDirection::Down),
    );
    let first = stack_split(
        &server,
        caller.raw(),
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    let height = server.display(&above, "#{pane_height}");
    let second = stack_split(&server, first.raw(), SplitPlacement::Stacked);
    assert_eq!(server.display(&above, "#{pane_height}"), height);
    let third = stack_split(&server, first.raw(), SplitPlacement::Stacked);
    assert_eq!(server.display(&above, "#{pane_height}"), height);
    assert_stack(
        &server,
        &[first.raw().into(), second.raw().into(), third.raw().into()],
        third.raw(),
    );
}

#[test]
fn lone_stack_member_restart_preserves_panes_above_the_caller() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    for pane in &members[1..] {
        server.tmux(&["kill-pane", "-t", pane]);
    }
    let above = server.stdout(&[
        "split-window",
        "-v",
        "-f",
        "-b",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &caller,
        "sleep 600",
    ]);
    let height = server.display(&above, "#{pane_height}");
    let replacement = stack_split(
        &server,
        &members[0],
        SplitPlacement::Rejoin(rimz::mux::SplitDirection::Right),
    );
    assert_eq!(server.display(&above, "#{pane_height}"), height);
    assert_eq!(
        server.display(replacement.raw(), "#{@rimz_stack}"),
        server.display(&members[0], "#{@rimz_stack}")
    );
    server.tmux(&["kill-pane", "-t", &members[0]]);
    assert_eq!(server.display(&above, "#{pane_height}"), height);
    assert_stack(&server, &[replacement.raw().into()], replacement.raw());
}

#[test]
fn stack_borders_target_the_background_window() {
    require_tmux!();
    let server = TmuxServer::new();
    ensure_rimz_session(&server, "stacks", Some((160, 60)));
    let current = server.display("stacks", "#{pane_id}");
    let background = server.stdout(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "stacks",
        "sleep 600",
    ]);
    let new = stack_split(&server, &background, SplitPlacement::Stacked);
    assert_stack(
        &server,
        &[background.clone(), new.raw().into()],
        &background,
    );
    assert_eq!(server.display(&background, "#{pane-border-status}"), "top");
    assert_eq!(
        server.display(&background, "#{@rimz_stack_borders_owned}"),
        "1"
    );
    assert!(
        server
            .display(&background, "#{pane-border-format}")
            .contains("@rimz_stack")
    );
    let local = server.stdout(&["show-options", "-w", "-t", &current]);
    assert!(
        !local.contains("pane-border-status")
            && !local.contains("pane-border-format")
            && !local.contains("@rimz_stack_borders_owned"),
        "{local}"
    );
    assert_eq!(server.display("stacks", "#{pane_id}"), current);
}

#[test]
fn stack_borders_reapply_and_hand_back_without_touching_other_windows() {
    require_tmux!();
    let server = TmuxServer::new();
    let (caller, members) = stack_fixture(&server);
    assert_eq!(server.display(&caller, "#{pane-border-status}"), "top");
    let border = server.display(&caller, "#{pane-border-format}");
    assert!(
        border.contains("@rimz_stack") && border.contains("@rimz_title"),
        "{border}"
    );
    let other = server.stdout(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "stacks",
        "sleep 600",
    ]);
    ensure_rimz_session(&server, "stacks", Some((160, 60)));
    assert_eq!(server.display(&caller, "#{pane-border-status}"), "top");
    assert_eq!(server.display(&other, "#{pane-border-status}"), "off");
    for pane in &members {
        server.tmux(&["kill-pane", "-t", pane]);
    }
    let local = server.stdout(&["show-options", "-w", "-t", &caller]);
    assert!(
        !local.contains("pane-border-status") && !local.contains("pane-border-format"),
        "{local}"
    );
}

#[test]
fn explicit_border_off_wins_over_stack_defaults() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().unwrap();
    let mut opts = session_opts(
        "stacks",
        WorkspaceId::from_project_root(cwd.path()),
        cwd.path(),
        cwd.path(),
        Some((160, 60)),
    );
    opts.config.tmux = toml::from_str("pane_border_status = 'off'\n").unwrap();
    server.backend.ensure_session(&opts).unwrap();
    let caller = server.display("stacks", "#{pane_id}");
    let first = stack_split(
        &server,
        &caller,
        SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
    );
    let second = stack_split(&server, first.raw(), SplitPlacement::Stacked);
    assert_stack(
        &server,
        &[first.raw().into(), second.raw().into()],
        second.raw(),
    );
    assert_eq!(server.display(&caller, "#{pane-border-status}"), "off");
    assert!(
        !server
            .stdout(&["show-options", "-w", "-t", &caller])
            .contains("pane-border-format")
    );
}

#[test]
fn stack_hooks_preserve_user_global_hooks_and_companion_grid_refuses_stacks() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("stacks");
    server.tmux(&[
        "set-hook",
        "-g",
        "window-pane-changed",
        "set-option -g @probe 1",
    ]);
    let (_, members) = stack_fixture(&server);
    server.tmux(&["select-pane", "-t", &members[0]]);
    assert_stack(&server, &members, &members[0]);
    assert_eq!(server.stdout(&["show-options", "-gv", "@probe"]), "1");
    let result = server
        .backend
        .append_companion_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "stacks".into(),
                pane_id: PaneId::from_parts(MuxName::Tmux, &members[0]),
            },
            cwd: None,
            command: Some(vec!["sleep".into(), "600".into()]),
            title: None,
            close_on_exit: true,
            env: Default::default(),
            placement: SplitPlacement::Stacked,
            focus: false,
        })
        .unwrap();
    assert_eq!(result, rimz::mux::CompanionPaneAppend::Full);
}

#[test]
fn stacked_layout_birth_opens_focus_or_last_in_each_column() {
    require_tmux!();
    let server = TmuxServer::new();
    ensure_rimz_session(&server, "stacks", Some((240, 70)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = sidebar_opts(server._tempdir.path(), "stacks", stub, Some(240));
    server.backend.open_sidebar(&sidebar, None).unwrap();
    let columns = (0..2)
        .map(|_| LayoutColumn {
            stacked: true,
            panes: (0..3)
                .map(|_| PaneCmd {
                    argv: vec!["sleep".into(), "600".into()],
                    name: None,
                })
                .collect(),
        })
        .collect();
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "birth".into(),
            panes: LayoutPanes {
                columns,
                focused_pane: 4,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar,
        })
        .unwrap();
    let panes = server.wait_for_panes("stacks:birth", 7);
    let mut columns = BTreeMap::<u64, Vec<PaneGeom>>::new();
    for pane in panes.into_iter().filter(|pane| pane.left > 0) {
        columns.entry(pane.left).or_default().push(pane);
    }
    assert_eq!(columns.len(), 2);
    for (index, column) in columns.values_mut().enumerate() {
        column.sort_by_key(|pane| pane.top);
        let ids = column
            .iter()
            .map(|pane| pane.id.clone())
            .collect::<Vec<_>>();
        assert_stack(&server, &ids, &ids[if index == 1 { 1 } else { 2 }]);
    }
}

#[test]
fn open_tab_after_anchor_inserts_next_to_it() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    let session = "rimz-tab-anchor";
    server
        .backend
        .ensure_session(&session_opts(
            session,
            WorkspaceId::from_project_root(cwd.path()),
            cwd.path(),
            cwd.path(),
            Some((120, 40)),
        ))
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts(server._tempdir.path(), session, stub, Some(120))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    server.tmux(&["rename-window", "-t", session, "anchor"]);
    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(session, "#{pane_id}"));
    server.tmux(&["new-window", "-d", "-t", session, "-n", "middle", "sh"]);
    server.tmux(&["new-window", "-d", "-t", session, "-n", "tail", "sh"]);
    server.tmux(&["select-window", "-t", &format!("{session}:tail")]);

    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "inserted".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: Some(anchor),
            sidebar,
        })
        .expect("open anchored tab");

    let windows = server.stdout(&[
        "list-windows",
        "-t",
        session,
        "-F",
        "#{window_index} #{window_name}",
    ]);
    let names = windows
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(_, name)| name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["anchor", "inserted", "middle", "tail"], "{windows}");
    assert_eq!(
        server.display(session, "#{window_name}"),
        "tail",
        "anchored focus: false must preserve the active window, not focus the anchor",
    );
}

#[test]
fn open_tab_can_suppress_hook_docked_sidebar() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&session_opts(
            "rimz-gallery",
            WorkspaceId::from_project_root(cwd.path()),
            cwd.path(),
            cwd.path(),
            Some((240, 40)),
        ))
        .expect("ensure_session");
    // Dock suppression must not depend on the renderer's async title escape.
    let (_stub_dir, stub) = delayed_sidebar_title_command_stub();
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts(server._tempdir.path(), "rimz-gallery", stub, Some(240))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "gallery".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: false,
            after: None,
            sidebar,
        })
        .expect("open_tab");
    let panes = server.wait_for_panes("rimz-gallery:gallery", 1);
    assert_eq!(
        panes.len(),
        1,
        "undocked tab should carry one work pane and no sidebar: {panes:?}",
    );
    let listed = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-gallery".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes;
    assert!(
        listed.iter().all(|pane| {
            pane.view_name.as_deref() != Some("gallery")
                || pane.command.as_deref() != Some("rimz-sidebar")
        }),
        "gallery window should not retain the hook-docked sidebar: {listed:?}",
    );
}

/// A tab opened while tmux's latest client is narrow is still laid out at the
/// full attached width: the hook sidebar is re-asserted to the birth width before
/// agent columns split, a stale caller verdict is ignored, and autosizing is
/// restored after the birth correction.

#[test]
fn open_tab_from_narrow_client_normalizes_to_full_width() {
    require_tmux!();
    let server = TmuxServer::new();
    let cwd = TempDir::new().expect("cwd tempdir");
    server
        .backend
        .ensure_session(&session_opts(
            "rimz-narrow-tab",
            WorkspaceId::from_project_root(cwd.path()),
            cwd.path(),
            cwd.path(),
            Some((300, 50)),
        ))
        .expect("ensure_session");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(WorkspaceId::from_project_root(cwd.path()), cwd.path())
                .expect("fixture state"),
            cwd.path(),
        ),
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts(server._tempdir.path(), "rimz-narrow-tab", stub, Some(300))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let _wide = AttachedTmuxClient::attach(&server.socket, "rimz-narrow-tab", 300, 50);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.client_widths("rimz-narrow-tab").contains(&300) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        server.client_widths("rimz-narrow-tab").contains(&300),
        "wide client should register before opening the tab"
    );
    let narrow = AttachedTmuxClient::attach(&server.socket, "rimz-narrow-tab", 150, 50);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let widths = server.client_widths("rimz-narrow-tab");
        if widths.contains(&300) && widths.contains(&150) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "both attached client widths should register, got {widths:?}",
        );
        thread::sleep(Duration::from_millis(25));
    }
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
        name: None,
    };
    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "float".to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                ],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar.clone(),
        })
        .expect("open_tab");
    drop(narrow);
    let target = "rimz-narrow-tab:float";
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let width = server.display(target, "#{window_width}");
        if width == "300" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "new tab should settle on the 300-col client, got {width}"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let panes = server.wait_for_panes(target, 3);
    assert_eq!(
        panes.len(),
        3,
        "tab should be born with a sidebar and two work panes: {panes:?}",
    );
    let sidebar_pane = panes
        .iter()
        .find(|pane| pane.left == 0)
        .expect("the hook-docked sidebar");
    let cap = u64::from(
        sidebar
            .target
            .cols(sidebar.detected_view_size.map(|(cols, _)| cols))
            .get(),
    );
    let lower = cap.saturating_sub(2);
    assert!(
        sidebar_pane.width >= lower && sidebar_pane.width <= cap,
        "sidebar should stay near capped {cap} cols instead of the stale caller width: {panes:?}",
    );
    let work: Vec<_> = panes.iter().filter(|pane| pane.left > 0).collect();
    assert_eq!(
        work.len(),
        2,
        "two work panes sit right of the sidebar: {panes:?}",
    );
    let diff = work[0].width.abs_diff(work[1].width);
    assert!(
        diff <= 2,
        "work columns should split evenly after normalization: {work:?}",
    );
}
