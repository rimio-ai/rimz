use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarWidth, TabOptions, WidthAdjust, WidthSyncOptions,
    ZellijBackend,
};
use tempfile::TempDir;

use super::support::*;

#[test]
fn sidebar_width_steps_resize_birth_and_explicit_layout_panes() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("widthstep");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = sidebar_opts(&name, cwd.path(), stub, 120);
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);
    let _client = AttachedClient::attach(xdg.path(), &name, 120, 40);
    wait_for_attached_client(xdg.path(), &name);
    let listed = raw_sidebar_pane(xdg.path(), &name);
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", listed.id));
    let initial = listed.pane_columns;

    backend
        .resize_sidebar_width(&name, &pane, WidthAdjust::Wider)
        .expect("widen sidebar");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while raw_sidebar_pane(xdg.path(), &name).pane_columns <= initial
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let wider = raw_sidebar_pane(xdg.path(), &name).pane_columns;
    assert!(
        wider > initial,
        "native wider step did not grow {initial}: {wider}"
    );

    backend
        .resize_sidebar_width(&name, &pane, WidthAdjust::Narrower)
        .expect("narrow sidebar");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while raw_sidebar_pane(xdg.path(), &name).pane_columns >= wider
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        raw_sidebar_pane(xdg.path(), &name).pane_columns < wider,
        "native narrower step did not shrink {wider}",
    );

    // Explicit `new-tab --layout` sidebars must also remain resizable. Zellij
    // pins panes whose KDL size is a bare integer, which was the old spelling
    // on this path even though template-born panes already used a percentage.
    let tab_name = "explicit width";
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: tab_name.to_owned(),
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
        .expect("open explicit tab");
    let explicit = wait_for_named_sidebar_pane(xdg.path(), &name, tab_name)
        .expect("explicit tab carries sidebar");
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", explicit.id));
    backend
        .resize_sidebar_width(&name, &pane, WidthAdjust::Wider)
        .expect("widen explicit-layout sidebar");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let resized = loop {
        let resized = wait_for_named_sidebar_pane(xdg.path(), &name, tab_name)
            .expect("explicit tab keeps sidebar");
        if resized.columns > explicit.columns || std::time::Instant::now() >= deadline {
            break resized;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(
        resized.columns > explicit.columns,
        "explicit-layout sidebar stayed resize-pinned at {} columns",
        explicit.columns,
    );
}

#[test]
fn sidebar_widths_converge_after_resize_new_tab_and_override() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("fixedw");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let width = SidebarWidth::default();
    let sidebar = sidebar_opts(&name, cwd.path(), stub, 340);
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    // The launch seed came from a 340-column terminal, but this attached client
    // is only 210 columns wide. The detached percentage seed therefore lands
    // near 44 columns while the live target is 63.
    let _client = AttachedClient::attach(xdg.path(), &name, 210, 60);
    wait_for_attached_client(xdg.path(), &name);
    write_topology_cache_from_list_panes(xdg.path(), &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg.path(), &sidebar.workspace_id, &name);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[42..=46]),
        "the capped launch seed rescales onto the smaller client, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    let mut sync = WidthSyncOptions {
        session_name: name.clone(),
        workspace_id: sidebar.workspace_id.clone(),
        width,
        width_override: None,
    };
    assert_eq!(
        backend
            .converge_sidebar_widths(&sync)
            .expect("converge attached birth"),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[53..=65]),
        "the birth pane converges near the smaller view's 63-column target, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A native tab starts from the policy percentage at the live client width.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[53..=65, 60..=65]),
        "the native template births the new tab near 30% of the live view, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
    assert_eq!(
        backend
            .converge_sidebar_widths(&sync)
            .expect("verify new tab target"),
        0,
    );

    // Keep one tab active while three existing tabs need the room override.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 3);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[53..=65, 60..=65, 60..=65]),
        "all three tabs start at policy width, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A room override becomes the target for every existing tab, including
    // the two background tabs, and every future tab.
    sync.width_override = std::num::NonZeroU16::new(40);
    assert_eq!(
        backend
            .converge_sidebar_widths(&sync)
            .expect("propagate override"),
        3,
    );
    assert!(wait_for_sidebar_columns(
        xdg.path(),
        &name,
        &[35..=45, 35..=45, 35..=45]
    ));
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 4);
    assert_eq!(
        backend
            .converge_sidebar_widths(&sync)
            .expect("converge overridden new tab"),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[35..=45, 35..=45, 35..=45, 35..=45]),
        "the override propagates to every tab, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
}
