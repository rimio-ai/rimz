use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, SidebarWidth, TabOptions, ZellijBackend};
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
        wait_for_sidebar_columns(xdg.path(), &name, &[69..=72]),
        "the attached session must land the birth sidebar within rounding of \
         the 72-column cap, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );

    // A user-opened tab instantiates the `new_tab_template` at live geometry:
    // the fixed spelling lands exactly at the cap.
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert!(
        wait_for_sidebar_columns(xdg.path(), &name, &[69..=72, 72..=72]),
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
        wait_for_sidebar_columns(xdg.path(), &name, &[69..=72, 72..=72, 69..=72]),
        "backend tab should mirror the live session width, not the stale \
         33-column caller verdict, got {:?}",
        sidebar_columns_by_tab(xdg.path(), &name),
    );
}
