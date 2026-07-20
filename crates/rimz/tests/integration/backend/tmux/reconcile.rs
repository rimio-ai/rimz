#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

#[test]
fn reconcile_without_client_or_probe_leaves_width_seed_alone() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-reconcile-no-width-basis";
    server.ensure_with_shell(session);
    assert_eq!(server.display(session, "#{window_width}"), "80");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(session, stub, None);
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    let sidebar = wait_for_sidebar_pane(&server, session, None);
    let option_before = server.show_option(&["-t", session], "@rimz_sidebar_cols");
    let width_before = server.display(sidebar.raw(), "#{pane_width}");
    server.tmux(&["set-hook", "-u", "-t", session, "after-new-window"]);

    server
        .backend
        .reconcile_sidebars(
            &opts,
            &rimz::mux::SidebarLiveness {
                claimed_panes: [sidebar.clone()].into(),
                ..Default::default()
            },
        )
        .expect("reconcile_sidebars");

    assert_eq!(
        server.show_option(&["-t", session], "@rimz_sidebar_cols"),
        option_before,
        "reconcile without honest geometry must preserve the hook width seed",
    );
    assert_eq!(
        server.display(sidebar.raw(), "#{pane_width}"),
        width_before,
        "reconcile without honest geometry must preserve live pane width",
    );
    assert!(
        server.has_after_new_window_hook(session),
        "structural reconcile still re-asserts the window hook",
    );
}

#[test]
fn reconcile_normalizes_detached_geometry_from_the_attach_probe() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-reconcile-attach-probe";
    server.ensure_with_shell(session);
    server.tmux(&["new-window", "-d", "-t", session, "-n", "second", "sh"]);
    assert!(
        server
            .stdout(&["list-windows", "-t", session, "-F", "#{window_width}"])
            .lines()
            .all(|width| width == "80"),
        "the detached fixture should begin at tmux's fictional default width",
    );
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(session, stub, Some(212));
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    let sidebar = wait_for_sidebar_pane(&server, session, None);

    let report = server
        .backend
        .reconcile_sidebars(
            &opts,
            &rimz::mux::SidebarLiveness {
                claimed_panes: [sidebar.clone()].into(),
                ..Default::default()
            },
        )
        .expect("reconcile_sidebars");

    assert_eq!(report.recovered, 1, "the second window gains a sidebar");
    assert_eq!(
        server.show_option(&["-t", session], "default-size"),
        "212x50",
    );
    let window_ids = server.stdout(&["list-windows", "-t", session, "-F", "#{window_id}"]);
    for window_id in window_ids.lines() {
        assert_eq!(server.display(window_id, "#{window_width}"), "212");
        assert_ne!(
            server.show_option(&["-w", "-t", window_id], "window-size"),
            "manual",
            "normalized windows must resume tracking attached clients",
        );
    }
    assert_eq!(
        server.show_option(&["-t", session], "@rimz_sidebar_cols"),
        "53",
    );
    let sidebars = sidebar_pane_ids(&server, session, None);
    assert_eq!(sidebars.len(), 2, "every window should carry one sidebar");
    for sidebar in sidebars {
        assert_eq!(server.display(sidebar.raw(), "#{pane_width}"), "53");
    }
}

#[test]
fn reconcile_repairs_sidebar_width_outside_the_shared_band() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-width-repair";
    ensure_rimz_session(&server, session, Some((240, 50)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(session, stub, Some(240));
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    let sidebar = wait_for_sidebar_pane(&server, session, None);
    let liveness = rimz::mux::SidebarLiveness {
        claimed_panes: [sidebar.clone()].into(),
        ..Default::default()
    };
    server.tmux(&["resize-pane", "-t", sidebar.raw(), "-x", "90"]);
    assert_eq!(server.display(sidebar.raw(), "#{pane_width}"), "90");
    let report = server
        .backend
        .reconcile_sidebars(&opts, &liveness)
        .expect("reconcile wide sidebar");
    assert_eq!(report.redocked, 1);
    assert_eq!(
        server.display(sidebar.raw(), "#{pane_width}"),
        "60",
        "an out-of-band sidebar snaps exactly to the birth verdict",
    );
    server.tmux(&["resize-pane", "-t", sidebar.raw(), "-x", "74"]);
    let report = server
        .backend
        .reconcile_sidebars(&opts, &liveness)
        .expect("reconcile manually resized sidebar");
    assert_eq!(report.redocked, 1);
    assert_eq!(
        server.display(sidebar.raw(), "#{pane_width}"),
        "60",
        "a native manual resize beyond the exact backend's band snaps back",
    );
}

#[test]
fn reconcile_sidebars_ignores_other_tmux_sessions() {
    require_tmux!();
    let session_a = "rimz-reconcile-scope-a";
    let session_b = "rimz-reconcile-scope-b";
    let server = TmuxServer::new();
    server.ensure_with_shell(session_a);
    server.ensure_with_shell(session_b);
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts_a = sidebar_opts(session_a, stub.clone(), Some(80));
    let opts_b = sidebar_opts(session_b, stub, Some(80));
    server
        .backend
        .open_sidebar(&opts_a, None)
        .expect("open_sidebar a");
    server
        .backend
        .open_sidebar(&opts_b, None)
        .expect("open_sidebar b");
    let a_claimed = wait_for_sidebar_pane(&server, session_a, None);
    let b_sidebar_before = wait_for_sidebar_pane(&server, session_b, None);
    server.tmux(&["new-window", "-d", "-t", session_a, "-n", "corrupted", "sh"]);
    let corrupt_target = format!("{session_a}:corrupted");
    let corrupt_window = server.display(&corrupt_target, "#{window_id}");
    let hook_sidebar = wait_for_sidebar_pane(&server, session_a, Some(&corrupt_window));
    server.tmux(&["kill-pane", "-t", hook_sidebar.raw()]);
    let work_pane = list_session_panes(&server, session_a)
        .into_iter()
        .find(|pane| {
            pane.view_id.as_deref() == Some(corrupt_window.as_str())
                && pane.command.as_deref() != Some("rimz-sidebar")
        })
        .expect("corrupted window work pane");
    let foreign_command = vec![
        opts_b.rimz_bin.to_string_lossy().into_owned(),
        "sidebar".to_owned(),
        "serve".to_owned(),
        "--mux".to_owned(),
        "tmux".to_owned(),
        "--workspace-id".to_owned(),
        opts_b.workspace_id.as_str().to_owned(),
        "--session-name".to_owned(),
        opts_b.session_name.clone(),
    ];
    server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Pane(work_pane.pane_id),
            cwd: None,
            command: Some(foreign_command),
            title: None,
            close_on_exit: false,
            env: BTreeMap::new(),
            placement: SplitPlacement::default(),
            focus: false,
        })
        .expect("plant foreign sidebar");
    let foreign_sidebar = wait_for_sidebar_pane(&server, session_a, Some(&corrupt_window));
    let report = server
        .backend
        .reconcile_sidebars(
            &opts_a,
            &rimz::mux::SidebarLiveness {
                claimed_panes: [a_claimed].into(),
                ..Default::default()
            },
        )
        .expect("reconcile_sidebars");
    assert_eq!(report.closed, 1, "foreign same-window sidebar is closed");
    assert_eq!(
        report.recovered, 1,
        "session A window regains its own sidebar"
    );
    assert_eq!(report.failed, 0);
    let healed_sidebar = wait_for_sidebar_pane(&server, session_a, Some(&corrupt_window));
    assert_ne!(
        healed_sidebar, foreign_sidebar,
        "the planted foreign sidebar should be replaced in session A",
    );
    assert!(
        !list_session_panes(&server, session_a)
            .iter()
            .any(|pane| pane.pane_id == foreign_sidebar),
        "the foreign pane id should be gone from session A",
    );
    assert_eq!(
        sidebar_pane_ids(&server, session_b, None),
        vec![b_sidebar_before],
        "session B's sidebar pane should not be closed or duplicated",
    );
}

#[test]
fn reconcile_sidebars_redocks_sidebar_without_skewing_work_columns() {
    require_tmux!();
    let session = "rimz-reconcile-no-skew";
    let target = format!("{session}:0");
    let server = TmuxServer::new();
    let width = SidebarWidth::default();
    let birth_size = width.birth_size(Some(80));
    let sidebar_cols = birth_size.cols.to_string();
    server.ensure_with_shell(session);
    let main_pane = server.display(&target, "#{pane_id}");
    server.tmux(&[
        "split-window",
        "-h",
        "-b",
        "-l",
        &sidebar_cols,
        "-t",
        &main_pane,
    ]);
    let sidebar_pane = server.display(&target, "#{pane_id}");
    server.tmux(&["split-window", "-h", "-t", &main_pane]);
    let right_pane = server.display(&target, "#{pane_id}");
    server.tmux(&["split-window", "-v", "-t", &right_pane]);
    server.tmux(&["kill-pane", "-t", &sidebar_pane]);
    let initial_panes = server.wait_for_panes(&target, 3);
    assert_eq!(
        initial_panes.len(),
        3,
        "test setup should remove the sidebar from a main/right-column layout: {initial_panes:?}",
    );
    let full_top = initial_panes
        .iter()
        .map(|pane| pane.top)
        .min()
        .expect("top pane");
    let full_height = initial_panes
        .iter()
        .map(|pane| pane.top + pane.height)
        .max()
        .expect("bottom pane")
        - full_top;
    assert!(
        server
            .display(&target, "#{pane_top}")
            .parse::<u64>()
            .expect("active pane top")
            > full_top,
        "test setup should leave the bottom pane active"
    );
    let absorbed_main = initial_panes
        .iter()
        .find(|pane| pane.left == 0 && pane.height == full_height)
        .expect("full-height main pane after sidebar removal");
    let right_width = initial_panes
        .iter()
        .filter(|pane| pane.left > absorbed_main.left)
        .map(|pane| pane.width)
        .max()
        .expect("right-column pane after sidebar removal");
    assert!(
        absorbed_main.width > right_width,
        "test setup should give the main pane the removed sidebar columns, got main={} right={} panes={initial_panes:?}",
        absorbed_main.width,
        right_width,
    );
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        birth_size,
        detected_view_size: Some((80, 24)),
        ..sidebar_opts(session, stub, Some(80))
    };
    let report = server
        .backend
        .reconcile_sidebars(&opts, &rimz::mux::SidebarLiveness::default())
        .expect("reconcile_sidebars");
    assert_eq!(report.recovered, 1);
    server.wait_for_pane_command(session, "rimz-sidebar");
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    let sidebar = panes
        .iter()
        .find(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
        .expect("sidebar pane");
    let raw_id = sidebar.pane_id.raw();
    assert_eq!(
        server
            .display(raw_id, "#{pane_left}")
            .parse::<u64>()
            .expect("sidebar left"),
        0
    );
    assert_eq!(
        server
            .display(raw_id, "#{pane_top}")
            .parse::<u64>()
            .expect("sidebar top"),
        full_top
    );
    assert_eq!(
        server
            .display(raw_id, "#{pane_height}")
            .parse::<u64>()
            .expect("sidebar height"),
        full_height
    );
    let healed_panes = server.wait_for_panes(&target, 4);
    let work_panes: Vec<_> = healed_panes
        .iter()
        .filter(|pane| pane.id.as_str() != raw_id)
        .collect();
    assert_eq!(
        work_panes.len(),
        3,
        "healed window should have three work panes plus the sidebar: {healed_panes:?}",
    );
    let main = work_panes
        .iter()
        .copied()
        .find(|pane| pane.left > 0 && pane.top == full_top && pane.height == full_height)
        .expect("full-height main pane");
    let right_width = work_panes
        .iter()
        .copied()
        .filter(|pane| pane.left > main.left)
        .map(|pane| pane.width)
        .max()
        .expect("right-column pane");
    assert!(
        main.width.abs_diff(right_width) <= 1,
        "re-added sidebar should keep work columns even, got main={} right={} panes={healed_panes:?}",
        main.width,
        right_width,
    );
}

#[test]
fn reconcile_sidebars_collapses_an_orphan_sidebar_only_window() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("multi"); // window 0: a working `sh` pane
    server.tmux(&["rename-window", "-t", "multi:0", "room"]);
    // window 1: a lone sidebar-titled pane, no working sibling — the orphan.
    server.tmux(&[
        "new-window",
        "-t",
        "multi",
        "-n",
        "ghost",
        "printf '\\033]2;rimz-sidebar\\007'; exec sleep 600",
    ]);
    server.wait_for_pane_command("multi", "rimz-sidebar");
    assert_eq!(
        server.window_names("multi"),
        vec!["room".to_owned(), "ghost".to_owned()],
        "two windows before reconcile",
    );
    let (_rimz_dir, rimz_bin) = sidebar_command_stub();
    let report = server
        .backend
        .reconcile_sidebars(
            &SidebarPaneOptions {
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-orphan")),
                project_root: std::env::current_dir().expect("cwd"),
                cwd: std::env::current_dir().expect("cwd"),
                ..sidebar_opts("multi", rimz_bin, Some(80))
            },
            // No live sidebars known: the orphan's pane is unclaimed, so it closes.
            &rimz::mux::SidebarLiveness::default(),
        )
        .expect("reconcile_sidebars");
    assert_eq!(report.closed, 1, "the orphan's lone sidebar pane is closed");
    assert_eq!(report.recovered, 1, "the working window gains a sidebar");
    assert_eq!(report.failed, 0);
    assert_eq!(
        server.window_names("multi"),
        vec!["room".to_owned()],
        "the orphan window collapsed; the working window survives",
    );
}
