use std::path::Path;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarPaneOptions, SidebarWidth, TabOptions, ZellijBackend,
};
use tempfile::TempDir;

use super::support::*;

#[test]
fn open_tab_unfocused_restores_attached_client_focus() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("tabfocus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-tabfocus")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(200)),
        width_override: None,
        rimz_bin: stub,
        replace_existing: false,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);

    let source_tab = "focus source";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: source_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
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
            session_name: name.clone(),
            title: background_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
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

    let focused = wait_for_focused_client_pane(&backend, &name, &source_pane);
    assert_eq!(
        focused,
        vec![source_pane],
        "unfocused open_tab must return the attached client to the source pane: {focused:?}",
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
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(220)),
        width_override: None,
        rimz_bin: stub,
        replace_existing: false,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);
    let _client = AttachedClient::attach(xdg.path(), &name, 220, 40);
    wait_for_attached_client(xdg.path(), &name);

    let tab_name = "sidebar gallery";
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: tab_name.to_owned(),
            cwd: cwd.path().to_path_buf(),
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

/// Backend and native tabs keep the docked sidebar outside the user's work area,
/// while no-direction pane opens split the focused work pane and pane closes
/// return space to the survivor.
#[test]
fn native_focused_splits_preserve_sidebar_in_backend_and_native_tabs() {
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
        cwd: cwd.path().to_path_buf(),
        width,
        birth_size: width.birth_size(Some(298)),
        width_override: None,
        rimz_bin: stub,
        replace_existing: false,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let client_columns: u16 = 380;
    let client_rows: u16 = 46;
    let _client = AttachedClient::attach(xdg.path(), &name, client_columns, client_rows);
    wait_for_attached_client(xdg.path(), &name);
    write_topology_cache_from_list_panes(xdg.path(), &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg.path(), &sidebar.workspace_id, &name);
    let width_sync = rimz::mux::WidthSyncOptions {
        session_name: name.clone(),
        workspace_id: sidebar.workspace_id.clone(),
        width,
        width_override: None,
    };
    let work_pane = || PaneCmd {
        argv: vec!["sleep".to_owned(), "600".to_owned()],
    };

    let backend_tab = "backend work split";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: backend_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![work_pane()]),
                    tiled_column(vec![work_pane()]),
                ],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open backend tab layout");

    assert_work_panes_reopen_in_survivor_after_closing_first(
        &backend,
        &width_sync,
        xdg.path(),
        &name,
        backend_tab,
        cwd.path(),
        (client_columns, client_rows),
    );

    let overflow_tab = "backend overflow split";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: overflow_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
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
        .expect("open backend overflow tab layout");
    let overflow_work = wait_for_named_work_pane_count(xdg.path(), &name, overflow_tab, 3);
    let focused_before = overflow_work[0];
    let focused_before_id =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", focused_before.id));
    let focused = wait_for_focused_client_pane(&backend, &name, &focused_before_id);
    assert!(
        focused.iter().any(|pane| pane == &focused_before_id),
        "overflow tab should focus a work pane before native split; \
         focused client panes: {focused:?}",
    );
    let sidebar_before = wait_for_named_sidebar_pane(xdg.path(), &name, overflow_tab)
        .expect("overflow tab keeps its sidebar");
    assert_eq!(
        sidebar_before.x, 0,
        "overflow tab starts with the sidebar docked left: {sidebar_before:?}",
    );

    spawn_sleep_pane(xdg.path(), &name, cwd.path());
    let focused_bounds_hold_two_panes = |pane: &PaneGeometry| {
        pane.x + 2 >= focused_before.x
            && pane.y + 2 >= focused_before.y
            && pane.x + pane.columns <= focused_before.x + focused_before.columns + 2
            && pane.y + pane.rows <= focused_before.y + focused_before.rows + 2
    };
    let overflow_split =
        wait_for_named_work_pane_state(xdg.path(), &name, overflow_tab, 4, |work| {
            let work_stays_right_of_sidebar = work
                .iter()
                .all(|pane| pane.x >= sidebar_before.columns.saturating_sub(2));
            let sidebar_unchanged = named_sidebar_pane_geometry(xdg.path(), &name, overflow_tab)
                .ok()
                .flatten()
                .is_some_and(|sidebar| {
                    sidebar.x == sidebar_before.x
                        && sidebar.y == sidebar_before.y
                        && sidebar.columns == sidebar_before.columns
                        && sidebar.rows == sidebar_before.rows
                });
            let focused_pane_was_split = work
                .iter()
                .filter(|pane| focused_bounds_hold_two_panes(pane))
                .count()
                >= 2;
            work_stays_right_of_sidebar && sidebar_unchanged && focused_pane_was_split
        });
    assert!(
        overflow_split
            .iter()
            .filter(|pane| focused_bounds_hold_two_panes(pane))
            .count()
            >= 2,
        "overflow split should divide the focused pane, got {overflow_split:?}",
    );
    let sidebar_after = named_sidebar_pane_geometry(xdg.path(), &name, overflow_tab)
        .expect("list overflow sidebar")
        .expect("overflow tab keeps its sidebar");
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
        "no-direction overflow split must not split the sidebar: before \
         {sidebar_before:?}, after {sidebar_after:?}",
    );

    let before_tabs = tab_ids(xdg.path(), &name);
    open_new_tab(xdg.path(), &name);
    let native_tab = wait_for_new_tab_name(xdg.path(), &name, &before_tabs);
    wait_for_named_sidebar_pane(xdg.path(), &name, &native_tab)
        .expect("native tab should carry a sidebar");
    let native_work = wait_for_named_work_pane_count(xdg.path(), &name, &native_tab, 1);
    let native_work_id =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", native_work[0].id));
    let focused = wait_for_focused_client_pane(&backend, &name, &native_work_id);
    assert!(
        focused.iter().any(|pane| pane == &native_work_id),
        "native tab should become the attached client's active work pane before a \
         no-direction split; focused client panes: {focused:?}",
    );

    spawn_sleep_pane(xdg.path(), &name, cwd.path());
    let split = wait_for_named_work_pane_state(xdg.path(), &name, &native_tab, 2, |work| {
        work[0].columns.abs_diff(work[1].columns) <= 5
    });
    assert_eq!(
        split.len(),
        2,
        "native tab should split into two work panes: {split:?}",
    );
    let diff = split[0].columns.abs_diff(split[1].columns);
    assert!(
        diff <= 5,
        "native tab's first no-direction split should be even, got {split:?}",
    );

    assert_work_panes_reopen_in_survivor_after_closing_first(
        &backend,
        &width_sync,
        xdg.path(),
        &name,
        &native_tab,
        cwd.path(),
        (client_columns, client_rows),
    );
}
