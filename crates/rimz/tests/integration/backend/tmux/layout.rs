#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

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
    let sidebar = sidebar_opts("rimz-bgview", stub, Some(80));
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
    let sidebar = sidebar_opts("rimz-bg-repair", stub, Some(80));
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let view = rimz::mux::DaemonView {
        name: rimz::daemon_view::VIEW_NAME.to_owned(),
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
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts("rimz-tab", stub, Some(300))
    };
    // Installs the `after-new-window` hook so the new tab is born with a sidebar.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };
    server
        .backend
        .open_tab(&TabOptions {
            title: "work".to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    // Column 0: two tiled rows — the `new-window` pane plus a
                    // `-v` split, exercising the in-column anchor tracking.
                    tiled_column(vec![work_pane(), work_pane()]),
                    // Column 1: one pane to the right — the `-h` split path.
                    tiled_column(vec![work_pane()]),
                ],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open_tab");
    // The hook-docked sidebar plus three work panes.
    let panes = server.wait_for_panes("rimz-tab:work", 4);
    assert_eq!(
        panes.len(),
        4,
        "tab should be born with a sidebar and three work panes: {panes:?}",
    );
    // The hook-docked sidebar is the sole pane at the left edge.
    assert_eq!(
        panes.iter().filter(|p| p.left == 0).count(),
        1,
        "exactly one pane (the hook-docked sidebar) sits at the left edge: {panes:?}",
    );
    // The three work panes form two columns: column 0 stacked into two rows
    // (same left edge, different top edge), column 1 a single pane to the right.
    let work: Vec<_> = panes.iter().filter(|p| p.left > 0).collect();
    assert_eq!(
        work.len(),
        3,
        "three work panes sit right of the sidebar: {work:?}"
    );
    let column_left = work.iter().map(|p| p.left).min().expect("a work pane");
    let column0: Vec<_> = work.iter().filter(|p| p.left == column_left).collect();
    let column1: Vec<_> = work.iter().filter(|p| p.left > column_left).collect();
    assert_eq!(
        column0.len(),
        2,
        "column 0 splits into two tiled rows: {work:?}"
    );
    assert_ne!(
        column0[0].top, column0[1].top,
        "column 0's rows tile vertically — same left, different top: {work:?}",
    );
    assert_eq!(
        column1.len(),
        1,
        "column 1 is a single pane to the right of column 0: {work:?}",
    );
    // Every work pane runs in the requested cwd.
    let want_cwd = cwd.path().canonicalize().expect("canonicalize cwd");
    for pane in &work {
        assert_eq!(
            Path::new(&pane.path).canonicalize().ok().as_deref(),
            Some(want_cwd.as_path()),
            "each work pane runs in the tab cwd: {pane:?}",
        );
    }
    // `focus: true` made the new tab the session's current window.
    assert_eq!(
        server.display("rimz-tab", "#{window_name}"),
        "work",
        "focus: true should select the new window",
    );
    server
        .backend
        .open_tab(&TabOptions {
            title: "solo".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
            },
            focus: false,
            dock_sidebar: true,
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
        "work",
        "focus: false should leave the session on its previous window",
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
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts("rimz-gallery", stub, Some(240))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };
    server
        .backend
        .open_tab(&TabOptions {
            title: "gallery".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
            },
            focus: true,
            dock_sidebar: false,
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
        workspace_id: WorkspaceId::from_project_root(cwd.path()),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        ..sidebar_opts("rimz-narrow-tab", stub, Some(300))
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
    };
    server
        .backend
        .open_tab(&TabOptions {
            title: "float".to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                ],
            },
            focus: false,
            dock_sidebar: true,
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
    let cap = u64::from(sidebar.target.cols.get());
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
