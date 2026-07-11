use std::path::Path;
use std::time::Duration;

use rimz::ids::WorkspaceId;
use rimz::mux::{MuxBackend, SessionHealth, SidebarPaneOptions, SidebarWidth, ZellijBackend};
use tempfile::TempDir;

use super::support::*;

/// Zellij 0.44.3 suppresses terminal mouse reporting when an attach command
/// explicitly passes `options --mouse-mode true`. Rimz keeps the enabled case
/// implicit so clicks reach the tab bar and sidebar, while still applying the
/// rest of the room options.
#[test]
fn attach_command_keeps_terminal_mouse_reporting_enabled() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("mouse");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let spec = ZellijBackend::with_runtime_dir(xdg.path())
        .attach_command(&name, &rimz::config::MultiplexerConfig::default());
    assert!(
        !spec
            .args
            .windows(2)
            .any(|pair| pair[0] == "--mouse-mode" && pair[1] == "true"),
        "Zellij 0.44.3 disables mouse reporting for `--mouse-mode true`: {spec:?}",
    );

    let output = capture_pty_output(&spec, Duration::from_millis(900));
    assert!(
        output
            .windows(b"\x1b[?1006h".len())
            .any(|w| w == b"\x1b[?1006h")
            && output
                .windows(b"\x1b[?1000h".len())
                .any(|w| w == b"\x1b[?1000h"),
        "attach output did not enable terminal mouse reporting",
    );
}

/// `open_sidebar` births the full Zellij room shape once: left sidebar, focused
/// right terminal, bottom bar, running command panes, and a default tab template
/// that gives future tabs the same sidebar + terminal pair.
#[test]
fn open_sidebar_births_native_layout_and_template() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("sidebar");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    backend.open_sidebar(&opts, None).expect("open_sidebar");

    let panes = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        panes.len() >= 2,
        "layout should create a sidebar + terminal pane in {name}: {panes:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
    assert_session_has_bottom_bar(xdg.path(), &name);
    assert_sidebars_not_held(xdg.path(), &name, "initial tab");

    let template = new_tab_template_dump(xdg.path(), &name);
    assert!(
        template.contains("rimz-sidebar"),
        "new tab template should carry the sidebar pane:\n{template}",
    );
    assert!(
        template.contains("pane focus=true"),
        "new tab template should carry an explicit focused right terminal:\n{template}",
    );

    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    assert_sidebars_not_held(xdg.path(), &name, "new tab");

    for tab in tab_ids(xdg.path(), &name) {
        let terminals = nonplugin_titles_in_tab(xdg.path(), &name, tab);
        let has_sidebar = terminals.iter().any(|t| t == "rimz-sidebar");
        let has_terminal = terminals.iter().any(|t| t != "rimz-sidebar");
        assert!(
            has_sidebar && has_terminal,
            "tab {tab} should carry the sidebar and a right terminal, got {terminals:?}",
        );
        let focused = wait_for_focused_non_sidebar_title_in_tab(xdg.path(), &name, tab)
            .unwrap_or_else(|| panic!("tab {tab} has no focused terminal pane"));
        assert_ne!(
            focused, "rimz-sidebar",
            "tab {tab} focuses the sidebar; focus must land on the right terminal",
        );
    }

    let before_reopen = wait_for_pane_count(xdg.path(), &name, 4);
    backend
        .open_sidebar(&opts, None)
        .expect("second open_sidebar");
    let second = wait_for_pane_count(xdg.path(), &name, before_reopen.len());
    assert_eq!(
        second.len(),
        before_reopen.len(),
        "re-opening a live session must not add or drop panes: {second:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
}
/// The pre-attach health gate: an absent room is born clean and RUNNING
/// (`Reborn`), a probe of the resulting live room reports `Healthy`, and a second
/// gate call leaves the working panes untouched (`Healthy`, no rebirth). This is
/// the un-bypassable check that replaces the old "attach and hope" path.
#[test]
fn ensure_clean_session_births_running_then_is_idempotent() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("cleanroom");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-cleanroom")),
        project_root: cwd.path().to_path_buf(),
        cwd: cwd.path().to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        width_override: None,
        rimz_bin: stub,
        replace_existing: false,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg.path());

    // Absent → born clean and running.
    assert_eq!(
        backend
            .ensure_clean_session(&opts, None)
            .expect("ensure_clean_session births the absent room"),
        SessionHealth::Reborn,
    );
    let born = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        born.len() >= 2,
        "the gate should birth a sidebar + terminal pane: {born:?}",
    );
    // No pane is held at a "Waiting to run" prompt — the room came up running.
    assert_sidebars_not_held(xdg.path(), &name, "reborn room");

    // A read-only probe of the now-live, clean room reports healthy.
    assert_eq!(
        backend
            .probe_session_health(&name)
            .expect("probe a live clean room"),
        SessionHealth::Healthy,
    );

    // A clean live room is left untouched — the gate never rebirths working panes.
    assert_eq!(
        backend
            .ensure_clean_session(&opts, None)
            .expect("ensure_clean_session on a clean live room"),
        SessionHealth::Healthy,
    );
    let again = wait_for_pane_count(xdg.path(), &name, 2);
    assert_eq!(
        again.len(),
        born.len(),
        "the gate must not add or drop panes on a clean room: {again:?}",
    );
}

/// A *live* session that has no sidebar (the renderer self-closed or crashed
/// while the session itself survived, or a prior launch was skipped and the
/// session was born by a plain `attach --create`) must regain one on the next
/// `open_sidebar` — a sidebar-less rimz session is non-functional, and the
/// only way to place a left pane in Zellij is at session birth. Regression
/// test for "fresh `rimz .` shows a single full-width pane, no sidebar" on a
/// workspace whose session already existed without a sidebar.
#[test]
fn open_sidebar_heals_a_live_session_missing_its_sidebar() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("nosb");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // Birth a live session with a plain, sidebar-less layout. The pane runs a
    // long sleep so the unattached background session stays alive deterministically.
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let plain = wait_for_pane_count(xdg.path(), &name, 1);
    assert!(
        !plain.is_empty(),
        "plain session should have a pane before open_sidebar: {plain:?}",
    );

    // `open_sidebar` must heal it: tear the sidebar-less session down and
    // rebirth one that carries the sidebar.
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    write_topology_cache_from_list_panes(xdg.path(), &opts.workspace_id, &name);
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(&opts, None)
        .expect("open_sidebar");

    let healed = wait_for_pane_count(xdg.path(), &name, 2);
    assert!(
        healed.len() >= 2,
        "open_sidebar should rebirth a sidebar-less live session with a sidebar: {healed:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg.path(), &name);
}
