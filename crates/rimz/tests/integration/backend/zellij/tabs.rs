use std::path::Path;
use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, SidebarWidth, TabOptions, ZellijBackend,
};
use tempfile::TempDir;

use super::support::*;

#[test]
fn open_tab_unfocused_routes_input_back_to_source() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("tabfocus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-tabfocus"));
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: workspace_id.clone(),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(200)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let mut client = AttachedClient::attach(xdg.path(), &name, 200, 50);

    let source_tab = "focus source";
    let input_log = cwd.path().join("source-input.log");
    backend
        .open_tab(&TabOptions {
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
                }])],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open focused source tab");

    let source_panes = wait_for_named_work_pane_count(xdg.path(), &name, source_tab, 1);
    assert_eq!(
        source_panes.len(),
        1,
        "source tab should have one work pane: {source_panes:?}",
    );
    let source_pane =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", source_panes[0].id));
    let focused = wait_for_focused_client_pane(&backend, &name, &source_pane);
    assert_eq!(
        focused,
        vec![source_pane.clone()],
        "the attached client should focus the source tab before the regression step: {focused:?}",
    );

    let background_tab = "background run";
    backend
        .open_tab(&TabOptions {
            title: background_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                }])],
            },
            focus: false,
            dock_sidebar: true,
            sidebar,
        })
        .expect("open unfocused background tab");
    assert_eq!(
        wait_for_named_work_pane_count(xdg.path(), &name, background_tab, 1).len(),
        1,
        "background tab should open one work pane",
    );

    client.send_line("rimz-source-route");
    let routed = poll_until(
        Duration::from_secs(5),
        || std::fs::read_to_string(&input_log).map_err(|err| err.to_string()),
        |contents| contents.contains("rimz-source-route"),
        "unfocused tab input routed to source pane",
    );
    assert!(routed.contains("rimz-source-route"));

    let runtime = rimz::store::RuntimePaths::under(workspace_id, xdg.path()).expect("runtime");
    let intent = rimz::sidebar::focus_anchor::load(&runtime).expect("applied focus intent");
    assert_eq!(intent.pane_id, source_pane);
    assert_eq!(
        intent.state,
        rimz::sidebar::focus_anchor::FocusIntentState::Applied,
    );
}
#[test]
fn open_tab_can_omit_sidebar_for_gallery_layout() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("gallery");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-gallery")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(220)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);
    let _client = AttachedClient::attach(xdg.path(), &name, 220, 40);

    let tab_name = "sidebar gallery";
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };
    backend
        .open_tab(&TabOptions {
            title: tab_name.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![work_pane()])],
            },
            focus: true,
            dock_sidebar: false,
            sidebar,
        })
        .expect("open gallery tab");

    assert_eq!(
        wait_for_named_work_pane_count(xdg.path(), &name, tab_name, 1).len(),
        1,
        "gallery tab should hold one work pane",
    );
    assert_eq!(
        named_sidebar_pane_geometry(xdg.path(), &name, tab_name)
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

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("worksplit");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let width = SidebarWidth::default();
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-worksplit")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(298)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let client_columns: u16 = 380;
    let client_rows: u16 = 46;
    let mut client = AttachedClient::attach(xdg.path(), &name, client_columns, client_rows);
    write_topology_cache_from_list_panes(xdg.path(), &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg.path(), &sidebar.workspace_id, &name);
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };

    let split_tab = "backend focused split";
    backend
        .open_tab(&TabOptions {
            title: split_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                ],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open backend split tab layout");
    let work = wait_for_named_work_pane_state(xdg.path(), &name, split_tab, 3, |work| {
        work.iter().map(|pane| pane.x + pane.columns).max() == Some(u64::from(client_columns))
    });
    let focused_before = work[1];
    let focused_before_id =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", focused_before.id));
    focus_attached_client_pane_until(
        xdg.path(),
        &name,
        focused_before.id,
        "chosen work pane before native split",
        || client.press_alt('l'),
    );
    let focused = wait_for_focused_client_pane(&backend, &name, &focused_before_id);
    assert!(
        focused.iter().any(|pane| pane == &focused_before_id),
        "backend tab should focus the chosen work pane before native split; \
         focused client panes: {focused:?}",
    );
    let sidebar_before = wait_for_named_sidebar_pane(xdg.path(), &name, split_tab)
        .expect("backend tab keeps its sidebar");
    assert_eq!(
        sidebar_before.x, 0,
        "backend tab starts with the sidebar docked left: {sidebar_before:?}",
    );

    spawn_sleep_pane(xdg.path(), &name, cwd.path());
    let focused_bounds_hold_two_panes = |pane: &PaneGeometry| {
        pane.x + 2 >= focused_before.x
            && pane.y + 2 >= focused_before.y
            && pane.x + pane.columns <= focused_before.x + focused_before.columns + 2
            && pane.y + pane.rows <= focused_before.y + focused_before.rows + 2
    };
    let split = wait_for_named_work_pane_state(xdg.path(), &name, split_tab, 4, |work| {
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
        || named_sidebar_pane_geometry(xdg.path(), &name, split_tab),
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
    let sidebar_after = named_sidebar_pane_geometry(xdg.path(), &name, split_tab)
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
