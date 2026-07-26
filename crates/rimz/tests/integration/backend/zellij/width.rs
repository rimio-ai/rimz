use rimz::ids::{MuxName, PaneId};
use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, TabOptions, ZellijBackend};
use tempfile::TempDir;

use super::support::*;

#[test]
fn sidebar_width_steps_resize_birth_and_explicit_layout_panes() {
    require_zellij!();

    let room = LiveZellijSession::new("widthstep");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = sidebar_opts(&name, cwd.path(), stub, 120);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg, &name, 2);
    let _client = AttachedClient::attach(&room, 120, 40);
    let listed = raw_sidebar_pane(xdg, &name);
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", listed.id));
    let initial = listed.pane_columns;
    let initial_cols = u16::try_from(initial).expect("sidebar width fits u16");

    backend
        .nudge_sidebar_width(&name, &pane, initial_cols, u16::MAX)
        .expect("widen sidebar");
    let wider = wait_for_sidebar_columns_matching(
        xdg,
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
        xdg,
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
            title: tab_name.to_owned(),
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
    let explicit =
        wait_for_named_sidebar_pane(xdg, &name, tab_name).expect("explicit tab carries sidebar");
    let pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", explicit.id));
    let explicit_cols = u16::try_from(explicit.columns).expect("sidebar width fits u16");
    backend
        .nudge_sidebar_width(&name, &pane, explicit_cols, u16::MAX)
        .expect("widen explicit-layout sidebar");
    let resized = wait_for_sidebar_columns_matching(
        xdg,
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
fn sidebar_widths_converge_after_resize_new_tab_and_shared_target() {
    require_zellij!();

    let room = LiveZellijSession::new("fixedw");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = sidebar_opts(&name, cwd.path(), stub, 340);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);

    // The launch seed came from a 340-column terminal, but this attached client
    // is only 210 columns wide. The detached percentage seed therefore lands
    // near 44 columns while the live narrow-view target rounds up to 53.
    const VIEW_COLS: u64 = 210;
    let target_cols = u16::try_from(rimz::mux::SidebarWidth::default().target_cols(VIEW_COLS))
        .expect("target fits u16");
    let stop_step = VIEW_COLS.div_ceil(20);
    let target_band = || {
        u64::from(target_cols)
            ..=u64::from(target_cols)
                .saturating_add(stop_step)
                .saturating_sub(1)
    };
    let _client =
        AttachedClient::attach(&room, u16::try_from(VIEW_COLS).expect("view fits u16"), 60);
    write_topology_cache_from_list_panes(xdg, &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg, &sidebar.workspace_id, &name);
    assert!(
        wait_for_sidebar_columns(xdg, &name, &[42..=46]),
        "the capped launch seed rescales onto the smaller client, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );

    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg, &name, target_cols, stop_step),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg, &name, &[target_band()]),
        "the birth pane converges at or just above the smaller view's {target_cols}-column target, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );

    // A native tab inherits the 340-column launch probe's whole-percent layout share,
    // then live convergence applies the narrow-view policy.
    open_new_tab(xdg, &name);
    wait_for_tab_count(xdg, &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg, &name, &[target_band(), 42..=46]),
        "the native template births the new tab from the cap-aware launch seed, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg, &name, target_cols, stop_step),
        1,
    );
    assert!(
        wait_for_sidebar_columns(xdg, &name, &[target_band(), target_band()]),
        "both tabs converge at or just above the 25% live target, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );

    // Keep one tab active while the two converged tabs need the room target;
    // the launch-seeded tab is already within its tolerance.
    open_new_tab(xdg, &name);
    wait_for_tab_count(xdg, &name, 3);
    assert!(
        wait_for_sidebar_columns(xdg, &name, &[target_band(), target_band(), 42..=46]),
        "the new tab starts at the launch seed while converged tabs stay narrow, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );

    // A shared target applies to every existing tab, including the two
    // background tabs, and every future tab.
    let shared_band = || 40..=40_u64.saturating_add(stop_step).saturating_sub(1);
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg, &name, 40, stop_step),
        2,
    );
    assert!(wait_for_sidebar_columns(
        xdg,
        &name,
        &[shared_band(), shared_band(), shared_band()]
    ));
    open_new_tab(xdg, &name);
    wait_for_tab_count(xdg, &name, 4);
    assert_eq!(
        converge_each_sidebar_with_nudges(&backend, xdg, &name, 40, stop_step),
        0,
    );
    assert!(
        wait_for_sidebar_columns(
            xdg,
            &name,
            &[shared_band(), shared_band(), shared_band(), shared_band()],
        ),
        "the shared target propagates to every tab, got {:?}",
        sidebar_columns_by_tab(xdg, &name),
    );
}

fn converge_each_sidebar_with_nudges(
    backend: &ZellijBackend,
    xdg: &std::path::Path,
    session: &str,
    target_cols: u16,
    step_cols: u64,
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
            .filter(|pane| {
                pane.pane_columns < u64::from(target_cols)
                    || pane.pane_columns >= u64::from(target_cols).saturating_add(step_cols.max(1))
            })
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
