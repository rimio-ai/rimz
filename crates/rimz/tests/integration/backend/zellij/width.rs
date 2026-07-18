use rimz::ids::{MuxName, PaneId};
use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, TabOptions, ZellijBackend};
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
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);
    let _client = AttachedClient::attach(xdg.path(), &name, 120, 40);
    wait_for_attached_client(xdg.path(), &name);
    let listed = raw_sidebar_pane(xdg.path(), &name);
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", listed.id));
    let initial = listed.pane_columns;
    let initial_cols = u16::try_from(initial).expect("sidebar width fits u16");

    backend
        .nudge_sidebar_width(&name, &pane, initial_cols, u16::MAX)
        .expect("widen sidebar");
    let wider = wait_for_sidebar_columns_matching(
        xdg.path(),
        &name,
        listed.id,
        |columns| columns > initial,
        "native wider sidebar step",
    );
    assert!(
        wider > initial,
        "native wider step did not grow {initial}: {wider}"
    );

    let wider_cols = u16::try_from(wider).expect("sidebar width fits u16");
    backend
        .nudge_sidebar_width(&name, &pane, wider_cols, 1)
        .expect("narrow sidebar");
    let narrower = wait_for_sidebar_columns_matching(
        xdg.path(),
        &name,
        listed.id,
        |columns| columns < wider,
        "native narrower sidebar step",
    );
    assert!(
        narrower < wider,
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
    let explicit_cols = u16::try_from(explicit.columns).expect("sidebar width fits u16");
    backend
        .nudge_sidebar_width(&name, &pane, explicit_cols, u16::MAX)
        .expect("widen explicit-layout sidebar");
    let resized = wait_for_sidebar_columns_matching(
        xdg.path(),
        &name,
        explicit.id,
        |columns| columns > explicit.columns,
        "explicit-layout wider sidebar step",
    );
    assert!(
        resized > explicit.columns,
        "explicit-layout sidebar stayed resize-pinned at {} columns",
        explicit.columns,
    );
}

fn wait_for_sidebar_columns_matching(
    xdg: &std::path::Path,
    session: &str,
    pane_id: u64,
    mut ready: impl FnMut(u64) -> bool,
    label: &str,
) -> u64 {
    poll_until(
        std::time::Duration::from_secs(15),
        || {
            Ok(list_panes(xdg, session)?
                .panes
                .iter()
                .find(|pane| !pane.is_plugin && pane.id == pane_id)
                .map(|pane| pane.pane_columns))
        },
        |columns| columns.as_ref().is_some_and(|columns| ready(*columns)),
        label,
    )
    .unwrap_or_else(|| panic!("sidebar pane terminal_{pane_id} disappeared from {session}"))
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
    let sidebar = sidebar_opts(&name, cwd.path(), stub, 340);
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    // The launch seed came from a 340-column terminal, but this attached client
    // is only 210 columns wide. The detached percentage seed therefore lands
    // near 44 columns while the live narrow-view target is 52.
    let _client = AttachedClient::attach(xdg.path(), &name, 210, 60);
    wait_for_attached_client(xdg.path(), &name);
    write_topology_cache_from_list_panes(xdg.path(), &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg.path(), &sidebar.workspace_id, &name);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[42..=46]),
        "the capped launch seed rescales onto the smaller client, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg.path(), &name, 52, 5),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[47..=57]),
        "the birth pane converges near the smaller view's 52-column target, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A native tab inherits the 340-column launch probe's cap-aware 21% seed,
    // then live convergence applies the narrow-view policy.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[47..=57, 42..=46]),
        "the native template births the new tab from the cap-aware launch seed, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg.path(), &name, 52, 5),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[47..=57, 47..=57]),
        "both tabs converge near the 25% live target, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // Keep one tab active while the two converged tabs need the room override;
    // the launch-seeded tab is already within its tolerance.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 3);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[47..=57, 47..=57, 42..=46]),
        "the new tab starts at the launch seed while converged tabs stay narrow, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A room override becomes the target for every existing tab, including
    // the two background tabs, and every future tab.
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg.path(), &name, 40, 5),
        2,
    );
    assert!(wait_for_sidebar_columns(
        xdg.path(),
        &name,
        &[35..=45, 35..=45, 35..=45]
    ));
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 4);
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg.path(), &name, 40, 5),
        0,
    );
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[35..=45, 35..=45, 35..=45, 35..=45]),
        "the override propagates to every tab, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
}

fn converge_each_sidebar_with_nudges(
    backend: &ZellijBackend,
    xdg: &std::path::Path,
    session: &str,
    target_cols: u16,
    tolerance: u64,
) -> usize {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut resized = std::collections::HashSet::new();
    loop {
        let snapshot = list_panes(xdg, session).expect("list panes while nudging widths");
        let sidebars: Vec<_> = snapshot
            .panes
            .iter()
            .filter(|pane| pane.is_sidebar())
            .collect();
        let pending: Vec<_> = sidebars
            .into_iter()
            .filter(|pane| pane.pane_columns.abs_diff(u64::from(target_cols)) > tolerance)
            .collect();
        if pending.is_empty() {
            return resized.len();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "sidebars did not converge to {target_cols} columns",
        );
        for pane in pending {
            let pane_id = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id));
            let current_cols = u16::try_from(pane.pane_columns).expect("sidebar width fits u16");
            backend
                .nudge_sidebar_width(session, &pane_id, current_cols, target_cols)
                .expect("nudge sidebar width");
            resized.insert(pane.id);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
