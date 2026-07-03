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
        birth_size: SidebarWidth::default().birth_size(Some(200)),
        rimz_bin: stub,
        replace_existing: false,
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
        birth_size: SidebarWidth::default().birth_size(Some(220)),
        rimz_bin: stub,
        replace_existing: false,
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

/// Backend and native tabs keep the fixed sidebar outside the user's work area.
/// Closing back to `sidebar | one work pane` and then opening a no-direction
/// terminal must split the work area, not rebalance a flat root that still
/// carries the fixed sidebar constraint.
#[test]
fn work_area_swap_layout_rebalances_backend_and_native_tabs() {
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
        birth_size: width.birth_size(Some(298)),
        rimz_bin: stub,
        replace_existing: false,
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

    let backend_tab = "backend work split";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: backend_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }]),
                    tiled_column(vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }]),
                ],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open backend tab layout");

    assert_work_panes_reopen_evenly_after_closing_first(
        xdg.path(),
        &name,
        backend_tab,
        cwd.path(),
        client_columns,
        client_rows,
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

    assert_work_panes_reopen_evenly_after_closing_first(
        xdg.path(),
        &name,
        &native_tab,
        cwd.path(),
        client_columns,
        client_rows,
    );
}
