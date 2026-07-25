use std::path::Path;
use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{MuxBackend, SessionHealth, SidebarPaneOptions, ZellijBackend};
use tempfile::TempDir;

use super::support::*;

/// Zellij 0.44.3 suppresses terminal mouse reporting when an attach command
/// explicitly passes `options --mouse-mode true`. RimZ keeps the enabled case
/// implicit so clicks reach the tab bar and sidebar, while still applying the
/// rest of the room options.
#[test]
fn attach_command_keeps_terminal_mouse_reporting_enabled() {
    require_zellij!();

    let room = LiveZellijSession::new("mouse");
    let xdg = room.path();
    let name = room.name().to_owned();
    let spec = ZellijBackend::with_runtime_dir(xdg)
        .attach_command(&name, &rimz::config::MultiplexerConfig::default());
    assert!(
        !spec
            .args
            .windows(2)
            .any(|pair| pair[0] == "--mouse-mode" && pair[1] == "true"),
        "Zellij 0.44.3 disables mouse reporting for `--mouse-mode true`: {spec:?}",
    );

    let output = capture_pty_output_until(&spec, Duration::from_secs(10), mouse_reporting_enabled);
    assert!(
        mouse_reporting_enabled(&output),
        "attach output did not enable terminal mouse reporting: {:?}",
        String::from_utf8_lossy(&output),
    );
}

fn mouse_reporting_enabled(output: &[u8]) -> bool {
    output
        .windows(b"\x1b[?1006h".len())
        .any(|window| window == b"\x1b[?1006h")
        && output
            .windows(b"\x1b[?1000h".len())
            .any(|window| window == b"\x1b[?1000h")
}

/// `open_sidebar` births the full Zellij room shape once: left sidebar, focused
/// right terminal, bottom bar, running command panes, and a default tab template
/// that gives future tabs the same sidebar + terminal pair.
#[test]
fn open_sidebar_births_native_layout_and_template() {
    require_zellij!();

    let room = LiveZellijSession::new("sidebar");
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "").expect("disable zsh first-run menu");
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let mut opts = sidebar_opts(&name, cwd.path(), stub, 120);
    let runtime = rimz::RuntimePaths::under(opts.workspace_id.clone(), xdg).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let shim_dir = TempDir::new().expect("shim tempdir");
    let marker = cwd.path().join("copilot-env.txt");
    let copilot = shim_dir.path().join("copilot");
    std::fs::write(
        &copilot,
        format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$COPILOT_OTEL_FILE_EXPORTER_PATH\" \"$OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT\" > '{}'\n",
            marker.display()
        ),
    )
    .expect("write copilot shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&copilot, std::fs::Permissions::from_mode(0o755))
            .expect("chmod copilot shim");
    }
    opts.extra_env = std::collections::BTreeMap::from([
        (
            "COPILOT_OTEL_FILE_EXPORTER_PATH".to_owned(),
            runtime.copilot_otel_path().to_string_lossy().into_owned(),
        ),
        (
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT".to_owned(),
            "false".to_owned(),
        ),
    ]);
    publish_room_bin(xdg, &opts);
    backend.open_sidebar(&opts, None).expect("open_sidebar");

    let panes = wait_for_pane_count(xdg, &name, 2);
    assert!(
        panes.len() >= 2,
        "layout should create a sidebar + terminal pane in {name}: {panes:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg, &name);
    assert_session_has_bottom_bar(xdg, &name);
    assert_sidebars_not_held(xdg, &name, "initial tab");

    let work_pane = panes
        .iter()
        .find(|pane| pane.spawn_command.is_none())
        .expect("ordinary work shell")
        .pane_id
        .clone();
    let initial_tab_id = expect_list_panes(xdg, &name)
        .panes
        .iter()
        .find(|pane| pane.pane_ref(&name).pane_id == work_pane)
        .map(|pane| pane.tab_id)
        .expect("initial work tab");
    let mut client = AttachedClient::attach(&room, 120, 40);
    client.wait_until_focused(&work_pane, "birth work pane");
    client.assert_input_reaches(&work_pane, "birth work pane");
    backend
        .send_keys(&work_pane, copilot.to_string_lossy().as_ref())
        .expect("type direct copilot shim");
    backend
        .send_key(&work_pane, rimz::mux::NamedKey::Enter)
        .expect("run direct copilot shim");
    let expected_marker = format!("{}\nfalse\n", runtime.copilot_otel_path().display());
    let marker_text = poll_until(
        Duration::from_secs(15),
        || std::fs::read_to_string(&marker).map_err(|err| err.to_string()),
        |text| text == &expected_marker,
        "direct copilot shim marker",
    );
    let capture = backend
        .capture_pane(&work_pane, Some(20), false)
        .expect("capture direct copilot pane");
    assert!(
        marker.exists(),
        "direct copilot shim did not run; panes={panes:#?}; capture={capture:#?}",
    );
    assert_eq!(marker_text, expected_marker, "direct copilot shim output",);

    let template = new_tab_template_dump(xdg, &name);
    assert!(
        template.contains("rimz-sidebar"),
        "new tab template should carry the sidebar pane:\n{template}",
    );
    assert!(
        template.contains("pane focus=true"),
        "new tab template should carry an explicit focused right terminal:\n{template}",
    );

    open_new_tab(xdg, &name);
    wait_for_tab_count(xdg, &name, 2);
    assert_sidebars_not_held(xdg, &name, "new tab");

    let tabs = tab_ids(xdg, &name);
    for tab in &tabs {
        let terminals = nonplugin_titles_in_tab(xdg, &name, *tab);
        let has_sidebar = terminals.iter().any(|t| t == "rimz-sidebar");
        let has_terminal = terminals.iter().any(|t| t != "rimz-sidebar");
        assert!(
            has_sidebar && has_terminal,
            "tab {tab} should carry the sidebar and a right terminal, got {terminals:?}",
        );
    }
    let new_tab_id = tabs
        .into_iter()
        .find(|tab| *tab != initial_tab_id)
        .expect("new tab id");
    let new_work = expect_list_panes(xdg, &name)
        .panes
        .iter()
        .find(|pane| pane.tab_id == new_tab_id && pane.is_live_terminal() && !pane.is_sidebar())
        .map(|pane| pane.id)
        .expect("new tab work pane");
    let new_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{new_work}"));
    client.go_to_tab_until(2, &new_work, "new-tab template work pane");
    client.assert_input_reaches(&new_work, "new-tab template work pane");

    wait_for_pane_count(xdg, &name, 4);
}
/// The pre-attach health gate: an absent room is born clean and RUNNING
/// (`Reborn`), a probe of the resulting live room reports `Healthy`, and a second
/// gate call leaves the working panes untouched (`Healthy`, no rebirth). This is
/// the un-bypassable check that replaces the old "attach and hope" path.
#[test]
fn ensure_clean_session_births_running_then_is_idempotent() {
    require_zellij!();

    let room = LiveZellijSession::new("cleanroom");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-cleanroom")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(30).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &opts);

    // Absent → born clean and running.
    assert_eq!(
        backend
            .ensure_clean_session(&opts, None)
            .expect("ensure_clean_session births the absent room"),
        SessionHealth::Reborn,
    );
    let born = wait_for_pane_count(xdg, &name, 2);
    assert!(
        born.len() >= 2,
        "the gate should birth a sidebar + terminal pane: {born:?}",
    );
    // No pane is held at a "Waiting to run" prompt — the room came up running.
    assert_sidebars_not_held(xdg, &name, "reborn room");

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
    let again = wait_for_pane_count(xdg, &name, 2);
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

    let room = LiveZellijSession::new("nosb");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    // Birth a live session with a plain, sidebar-less layout. The pane runs a
    // long sleep so the unattached background session stays alive deterministically.
    room.create_plain_background(cwd.path(), "60");
    let plain = wait_for_pane_count(xdg, &name, 1);
    assert!(
        !plain.is_empty(),
        "plain session should have a pane before open_sidebar: {plain:?}",
    );

    // `open_sidebar` must heal it: tear the sidebar-less session down and
    // rebirth one that carries the sidebar.
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    publish_room_bin(xdg, &opts);
    write_topology_cache_from_list_panes(xdg, &opts.workspace_id, &name);
    ZellijBackend::with_runtime_dir(xdg)
        .open_sidebar(&opts, None)
        .expect("open_sidebar");

    let healed = wait_for_pane_count(xdg, &name, 2);
    assert!(
        healed.len() >= 2,
        "open_sidebar should rebirth a sidebar-less live session with a sidebar: {healed:?}",
    );
    assert_sidebar_is_left_thirty_percent(xdg, &name);
}
