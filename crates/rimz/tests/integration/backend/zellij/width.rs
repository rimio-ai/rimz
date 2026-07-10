use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    LayoutPanes, MuxBackend, PaneCmd, SidebarLiveness, SidebarWidth, TabOptions, ZellijBackend,
};
use tempfile::TempDir;

use super::support::*;

/// The live width verdict survives session birth, a native tab from
/// `new_tab_template`, and a backend-opened tab whose caller carries a stale
/// sidebar-width verdict.
#[test]
fn sidebar_width_verdict_survives_birth_template_and_backend_tabs() {
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

    // A real-size client: the detached birth geometry is tiny, so the cap only
    // shows once the session adopts the attaching terminal's 340 columns. The
    // birth tab is the derived 21% — within a column or two of the cap.
    let _client = AttachedClient::attach(xdg.path(), &name, 340, 80);
    wait_for_attached_client(xdg.path(), &name);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[71..=73]),
        "the attached session must land the birth sidebar within rounding of \
         the 72-column cap, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A user-opened tab instantiates the `new_tab_template` at live geometry:
    // the fixed spelling lands exactly at the cap.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[71..=73, 72..=72]),
        "a tab opened from an attached client must be born at exactly the \
         72-column cap, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A backend-opened tab targets an existing live session, so a stale caller
    // verdict from the invoking pane must not replace the session's birth
    // verdict.
    let mut stale_sidebar = sidebar.clone();
    stale_sidebar.birth_size = width.birth_size(Some(110));
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: "agents".to_owned(),
            cwd: cwd.path().to_path_buf(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "600".to_owned()],
                }])],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: stale_sidebar,
        })
        .expect("open_tab");

    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[71..=73, 72..=72, 69..=72]),
        "backend tab should mirror the live session width, not the stale \
         33-column caller verdict, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // Force the birth-tab sidebar well below the repair band. Reconcile keeps
    // the renderer and grows the pane until the first step that reaches or
    // crosses the canonical width.
    let panes = expect_list_panes_json(xdg.path(), &name);
    let sidebar_panes: Vec<&serde_json::Value> = panes
        .as_array()
        .expect("pane array")
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .collect();
    let birth_sidebar_id = sidebar_panes
        .iter()
        .min_by_key(|pane| pane.get("tab_id").and_then(|value| value.as_u64()))
        .and_then(|pane| pane.get("id"))
        .and_then(|value| value.as_u64())
        .expect("birth sidebar id");
    let liveness = SidebarLiveness {
        claimed_panes: sidebar_panes
            .iter()
            .filter_map(|pane| pane.get("id").and_then(|value| value.as_u64()))
            .map(|id| PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}")))
            .collect(),
        ..Default::default()
    };
    resize_pane_steps(xdg.path(), &name, birth_sidebar_id, "decrease", 3);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[1..=54, 1..=100, 1..=100]),
        "test setup must narrow the birth sidebar beyond the repair band, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    write_topology_cache_from_list_panes(xdg.path(), &sidebar.workspace_id, &name);
    let _mirror = topology_cache_mirror(xdg.path(), &sidebar.workspace_id, &name);
    let report = reconcile_until_converged(xdg.path(), &sidebar, &liveness);
    assert_eq!(report.redocked, 1, "the narrow sidebar grows in place");
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[72..=89, 72..=72, 69..=72]),
        "the repaired sidebar must land inside one Zellij step of the verdict, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
}
