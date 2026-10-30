use std::path::Path;
use std::time::{Duration, Instant};

use rimz::ids::WorkspaceId;
use rimz::mux::{DaemonView, HostPane, MuxBackend, SidebarPaneOptions, ZellijBackend};
use rimz::pane::PaneRef;
use tempfile::TempDir;

use crate::common::CommandTimeoutExt;

use super::support::*;

/// A `BackgroundViewOptions` for a session whose content and runtime panes are
/// long-lived `sleep` commands and whose sidebar runs the alive-keeping `stub`,
/// so the launched tab is a faithful `sidebar | content | runtime`.
fn background_view_opts(session: &str, stub: &Path) -> rimz::mux::BackgroundViewOptions {
    rimz::mux::BackgroundViewOptions {
        view: DaemonView {
            name: "rimzd".to_owned(),
            content: vec![rimz::mux::HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: std::env::temp_dir(),
            }],
            hosts: vec![rimz::mux::HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: std::env::temp_dir(),
            }],
            loop_panel: rimz::mux::HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: std::env::temp_dir(),
            },
        },
        sidebar: SidebarPaneOptions {
            runtime: rimz::RuntimePaths::for_state_under(
                &rimz::StatePaths::under(
                    WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgview")),
                    stub.parent().expect("stub directory"),
                )
                .expect("fixture state"),
                stub.parent().expect("stub directory"),
            ),
            session_name: session.to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgview")),
            project_root: std::env::temp_dir(),
            extra_env: Default::default(),
            cwd: std::env::temp_dir(),
            target: rimz::mux::SidebarTarget {
                share: rimz::mux::WidthPermille::from_percent(25),
                max_cols: std::num::NonZeroU16::new(30).expect("nonzero test width"),
                pinned: false,
            },
            max_percent: 50,
            detected_view_size: None,
            rimz_bin: stub.to_path_buf(),
            pristine_birth: false,
            config: rimz::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        },
    }
}

#[test]
fn open_background_view_defers_when_the_session_is_detached() {
    require_zellij!();
    let room = LiveZellijSession::new("bgdefer");
    let cwd = TempDir::new().unwrap();
    room.create_plain_background(cwd.path(), "120");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = background_view_opts(room.name(), &stub);
    let before = PaneSnapshot::expect(room.path(), room.name());
    let launch = room.backend().open_background_view(&opts);
    assert!(
        matches!(launch, Ok(rimz::mux::BackgroundViewLaunch::Deferred)),
        "{launch:?}",
    );
    let after = PaneSnapshot::expect(room.path(), room.name());
    assert_eq!(after.tab_ids(), before.tab_ids(), "{after:?}");
    let output = room
        .command()
        .args(["--session", room.name(), "action", "list-tabs", "--json"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let tabs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(tabs.len(), 1, "no partial tab was created: {tabs:?}");
    assert_eq!(
        after.panes.iter().filter(|pane| !pane.is_plugin).count(),
        1,
        "{after:?}",
    );
}

#[test]
fn explicit_repair_reopens_a_closed_daemon_view_without_moving_focus() {
    require_zellij!();
    let room = LiveZellijSession::new("bgreopen");
    let name = room.name().to_owned();
    let cwd = TempDir::new().unwrap();
    room.create_plain_background(cwd.path(), "120");
    let client = AttachedClient::attach(&room, 120, 40);
    let backend = ZellijBackend::with_runtime_dir(room.path());
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = background_view_opts(&name, &stub);
    backend.open_background_view(&opts).unwrap();
    wait_for_tab_pane_count(room.path(), &name, "rimzd", 4);
    let tab = PaneSnapshot::expect(room.path(), &name)
        .panes
        .into_iter()
        .find(|pane| pane.tab_name.as_deref() == Some("rimzd"))
        .unwrap()
        .tab_id;
    let output = room
        .command()
        .args([
            "--session",
            &name,
            "action",
            "close-tab-by-id",
            &tab.to_string(),
        ])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), &name),
        |snapshot| !snapshot.panes.iter().any(|pane| pane.tab_id == tab),
        "closed daemon tab",
    );
    open_new_tab(room.path(), &name);
    let snapshot = PaneSnapshot::expect(room.path(), &name);
    let source_tab = *snapshot.tab_ids().last().unwrap();
    let source = snapshot
        .panes
        .iter()
        .find(|pane| pane.tab_id == source_tab && pane.is_live_terminal())
        .unwrap()
        .pane_ref(&name)
        .pane_id;
    client.wait_until_focused(&source, "non-leading source tab before reopen");
    let record: rimz::workspace::record::WorkspaceRecord =
        serde_json::from_value(serde_json::json!({
            "layout": 2,
            "workspace_id": opts.sidebar.workspace_id,
            "session_name": name,
            "project_root": opts.sidebar.project_root,
            "updated_at": "2026-01-01T00:00:00Z"
        }))
        .unwrap();
    assert_eq!(
        rimz::daemon_view::reopen_daemon_view(
            &backend,
            &opts.sidebar,
            || Some((record.clone(), opts.sidebar.rimz_bin.clone())),
            &rimz::config::MachineConfig::default(),
        ),
        rimz::daemon_view::ReopenOutcome::Reopened,
        "explicit repair must reopen a wholly closed view",
    );
    wait_for_tab_pane_count(room.path(), &name, "rimzd", 3);
    assert_eq!(client.view().viewed_panes, vec![source]);
}

/// `open_background_view` opens a dedicated, named tab born `sidebar | content |
/// runtime`, and is idempotent on that tab name: a second call launches nothing.
#[test]
fn open_background_view_creates_named_tab_idempotently() {
    require_zellij!();

    let room = LiveZellijSession::new("bgview");
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    room.create_plain_background(cwd.path(), "120");
    let _client = AttachedClient::attach(&room, 80, 24);
    let backend = ZellijBackend::with_runtime_dir(room.path());
    let (_stub_dir, stub) = sidebar_command_stub();

    let mut opts = background_view_opts(&name, &stub);
    opts.view = shape_view();

    let first = backend.open_background_view(&opts).expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    let panes = wait_for_tab_pane_count(room.path(), &name, "rimzd", 4);
    let geometry = backend
        .view_geometry(&name, &panes[0].pane_id, Duration::from_secs(3))
        .expect("view geometry");
    assert!(geometry.is_some(), "a fresh daemon tab has tiled geometry");
    let geometry = geometry.unwrap();
    assert_eq!(geometry.gap, 0);
    assert_eq!(geometry.panes.len(), 4, "the compact bar is not a terminal");
    let native = PaneSnapshot::load(room.path(), &name).unwrap();
    for rect in &geometry.panes {
        let pane = native
            .panes
            .iter()
            .find(|pane| pane.pane_ref(&name).pane_id == rect.pane_id && !pane.is_plugin)
            .expect("native terminal");
        assert_eq!(
            (rect.x, rect.y, rect.cols, rect.rows),
            (pane.pane_x, pane.pane_y, pane.pane_columns, pane.pane_rows),
        );
    }

    let second = backend.open_background_view(&opts).expect("second launch");
    assert_eq!(
        rimz::daemon_view::inspect_daemon_shape(&backend, &name, &opts.sidebar.workspace_id)
            .unwrap(),
        Some(rimz::daemon_view::DaemonShape::Healthy),
    );
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
    let anchor = &panes
        .iter()
        .find(|pane| pane.title.as_deref() != Some(rimz::pane::SIDEBAR_CHROME_TITLE))
        .unwrap()
        .pane_id;
    let toggle_fullscreen = || {
        let output = room
            .command()
            .args([
                "--session",
                &name,
                "action",
                "toggle-fullscreen",
                "--pane-id",
                anchor.raw(),
            ])
            .bounded_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    toggle_fullscreen();
    poll_until(
        Duration::from_secs(5),
        || {
            backend
                .view_geometry(&name, anchor, Duration::from_secs(3))
                .map_err(|err| err.to_string())
        },
        |geometry| geometry.is_none(),
        "fullscreen is not plain tiling",
    );
    toggle_fullscreen();
}

fn held_managed_pane(marker: &[&str]) -> HostPane {
    let mut argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "exec sleep 600".to_owned(),
    ];
    argv.extend(marker.iter().map(|arg| (*arg).to_owned()));
    HostPane {
        argv,
        cwd: std::env::temp_dir(),
    }
}

fn shape_view() -> DaemonView {
    DaemonView {
        name: rimz::pane::VIEW_NAME.to_owned(),
        content: vec![held_managed_pane(&[
            "rimz", "daemon", "content", "--slot", "0",
        ])],
        hosts: vec![held_managed_pane(&["rimz", "codex", "app-server", "serve"])],
        loop_panel: held_managed_pane(&["rimz", "loop", "watch", "--hold"]),
    }
}

#[test]
fn daemon_shape_repairs_only_the_spanning_loop_and_preserves_the_app_server() {
    daemon_shape_case(true);
}

#[test]
fn daemon_shape_leaves_content_over_loop_beside_a_full_height_app_server() {
    daemon_shape_case(false);
}

fn daemon_shape_case(repairable: bool) {
    use rimz::daemon_view::{DaemonShape, RepairOutcome, inspect_daemon_shape, repair_daemon_view};
    use rimz::mux::{
        LayoutColumn, LayoutPanes, PaneCmd, SplitDirection, SplitPaneOptions, SplitPlacement,
        SplitTarget, TabOptions,
    };

    require_zellij!();
    let room = LiveZellijSession::new("daemon-shape");
    room.create_background();
    let backend = room.backend();
    if backend
        .version()
        .unwrap()
        .split('.')
        .nth(1)
        .and_then(|v| v.parse::<u32>().ok())
        .is_none_or(|v| v < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    let _client = AttachedClient::attach(&room, 256, 81);
    let (stub_dir, stub) = sidebar_command_stub();
    let opts = background_view_opts(room.name(), &stub);
    let mut view = shape_view();
    let broker_pid_file = stub_dir.path().join("broker.pid");
    view.hosts[0].argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "printf '%s' \"$$\" > \"$1\"; exec sleep 600".to_owned(),
        "rimz codex app-server serve".to_owned(),
        broker_pid_file.to_string_lossy().into_owned(),
    ];
    backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: view.name.clone(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: view.content[0].argv.clone(),
                        name: None,
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            focus: false,
            dock_sidebar: true,
            after: None,
            sidebar: opts.sidebar.clone(),
        })
        .unwrap();
    let panes = wait_for_tab_pane_count(room.path(), room.name(), "rimzd", 2);
    let content = panes
        .iter()
        .find(|pane| pane.title.as_deref() != Some(rimz::pane::SIDEBAR_CHROME_TITLE))
        .unwrap()
        .pane_id
        .clone();
    let tab_id = PaneSnapshot::load(room.path(), room.name())
        .unwrap()
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.pane_ref(room.name()).pane_id == content)
        .unwrap()
        .tab_id;
    let split = |pane: &HostPane, direction| {
        backend
            .split_pane(SplitPaneOptions {
                target: SplitTarget::SessionPane {
                    session_name: room.name().to_owned(),
                    pane_id: content.clone(),
                },
                cwd: Some(pane.cwd.to_string_lossy().into_owned()),
                command: Some(pane.argv.clone()),
                title: Some(pane.argv.join(" ")),
                close_on_exit: true,
                placement: SplitPlacement::Directional(direction),
                focus: false,
                ..Default::default()
            })
            .unwrap()
    };
    if repairable {
        split(&view.loop_panel, SplitDirection::Down);
        split(&view.hosts[0], SplitDirection::Right);
    } else {
        split(&view.hosts[0], SplitDirection::Right);
        split(&view.loop_panel, SplitDirection::Down);
    }
    let before = poll_until(
        Duration::from_secs(10),
        || list_panes(room.path(), room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .filter(|pane| pane.tab_id == tab_id && !pane.is_plugin)
                .count()
                == 4
        },
        "daemon shape fixture",
    );
    let terminals = |snapshot: &PaneSnapshot| {
        let mut panes = snapshot
            .panes
            .iter()
            .filter(|pane| pane.tab_id == tab_id && !pane.is_plugin)
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>();
        panes.sort_by_key(|pane| pane.id);
        panes
    };
    let broker = before
        .panes
        .iter()
        .find(|pane| {
            !pane.is_plugin
                && pane.tab_id == tab_id
                && pane
                    .terminal_command
                    .as_deref()
                    .is_some_and(|cmd| cmd.contains("app-server"))
        })
        .unwrap();
    let broker_id = broker.id;
    let broker_pid = poll_until(
        Duration::from_secs(5),
        || Ok(std::fs::read_to_string(&broker_pid_file).unwrap_or_default()),
        |pid| pid.parse::<u32>().is_ok(),
        "app-server process id",
    );
    let shape = inspect_daemon_shape(backend, room.name(), &opts.sidebar.workspace_id).unwrap();
    if repairable {
        assert!(
            matches!(shape, Some(DaemonShape::Repairable { .. })),
            "{shape:?}; native rectangles: {:?}",
            terminals(&before)
        );
    } else {
        assert!(
            matches!(shape, Some(DaemonShape::Misshapen { .. })),
            "{shape:?}; native rectangles: {:?}",
            terminals(&before)
        );
    }
    assert_eq!(
        repair_daemon_view(backend, room.name(), &opts.sidebar.workspace_id, &view),
        RepairOutcome::Converged
    );
    let after = PaneSnapshot::load(room.path(), room.name()).unwrap();
    let broker_after = after
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == tab_id && pane.id == broker_id)
        .unwrap();
    assert!(
        broker_after.is_live_terminal(),
        "close-on-exit app server process survives"
    );
    assert_eq!(broker_after.terminal_command, broker.terminal_command);
    assert_eq!(
        std::fs::read_to_string(&broker_pid_file).unwrap(),
        broker_pid,
        "app server process is never restarted"
    );
    if !repairable {
        assert_eq!(
            terminals(&after),
            terminals(&before),
            "report-only must not touch panes"
        );
        return;
    }
    assert_eq!(
        inspect_daemon_shape(backend, room.name(), &opts.sidebar.workspace_id).unwrap(),
        Some(DaemonShape::Healthy)
    );
    let content_after = after
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.pane_ref(room.name()).pane_id == content)
        .unwrap();
    let panel = after
        .panes
        .iter()
        .find(|pane| {
            !pane.is_plugin
                && pane.tab_id == tab_id
                && pane
                    .terminal_command
                    .as_deref()
                    .is_some_and(|cmd| cmd.contains("loop watch"))
        })
        .unwrap();
    assert_eq!((content_after.pane_y, content_after.pane_rows), (0, 80));
    assert_eq!(
        (broker_after.pane_x, broker_after.pane_columns),
        (panel.pane_x, panel.pane_columns)
    );
    assert_eq!(broker_after.pane_y, 0);
    assert_eq!(broker_after.pane_rows, panel.pane_y);
    assert_eq!(panel.pane_y + panel.pane_rows, 80);
    assert!(panel.pane_x >= content_after.pane_x + content_after.pane_columns);
}

/// `open_sidebar` with a daemon view leads the session with the daemon tab.
/// Zellij can't reorder tabs after birth, so the session is born from a two-tab
/// layout — the daemon (`rimzd`) tab first, the focused working tab second — and
/// this asserts `rimzd` leads the resulting tab list.
#[test]
fn open_sidebar_with_a_daemon_leads_with_the_daemon_tab() {
    require_zellij!();

    let room = LiveZellijSession::new("bgfirst");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    let daemon = DaemonView {
        name: "rimzd".to_owned(),
        content: vec![HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: cwd.path().to_path_buf(),
        }],
        hosts: vec![HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: cwd.path().to_path_buf(),
        }],
        loop_panel: HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: cwd.path().to_path_buf(),
        },
    };
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let sidebar = SidebarPaneOptions {
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(
                WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgfirst")),
                cwd.path(),
            )
            .expect("fixture state"),
            cwd.path(),
        ),
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgfirst")),
        project_root: cwd.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: cwd.path().to_path_buf(),
        target: rimz::mux::SidebarTarget {
            share: rimz::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(30).expect("nonzero test width"),
            pinned: false,
        },
        max_percent: 50,
        detected_view_size: None,
        rimz_bin: stub,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    publish_room_bin(xdg, &sidebar);
    backend
        .open_sidebar(&sidebar, Some(&daemon))
        .expect("open_sidebar with daemon");

    assert!(
        wait_for_tab_named(&room, "rimzd"),
        "expected a rimzd tab after birth",
    );
    assert!(
        wait_for_first_tab(&room, "rimzd"),
        "daemon tab must lead the session; saw {:?}",
        tab_names_in_order(&room),
    );
    wait_for_tab_count(xdg, &name, 2);
    // Two tabs: the daemon tab and the working tab born beside it.
    let tab_names = poll_until(
        Duration::from_secs(10),
        || Ok(tab_names_in_order(&room)),
        |names| names.len() >= 2,
        "daemon and working tab names",
    );
    assert_eq!(
        tab_names.len(),
        2,
        "birth layout should produce exactly the daemon + working tabs: {tab_names:?}",
    );
    let panes = wait_for_tab_pane_count(xdg, &name, "rimzd", 4);
    assert_eq!(
        panes.len(),
        4,
        "rimzd should be born sidebar | content | runtime: {panes:?}",
    );
}

/// The session's tab names in tab order (`query-tab-names` prints one per line).
fn tab_names_in_order(session: &LiveZellijSession) -> Vec<String> {
    let out = session
        .command()
        .args(["--session", session.name(), "action", "query-tab-names"])
        .bounded_output();
    out.ok()
        .filter(|out| out.status.success())
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Poll `list_panes` until `tab_name` has `want` terminal panes, or time out.
fn wait_for_tab_pane_count(xdg: &Path, session: &str, tab_name: &str, want: usize) -> Vec<PaneRef> {
    poll_until(
        Duration::from_secs(20),
        || {
            Ok(list_panes(xdg, session)?
                .pane_refs()
                .into_iter()
                .filter(|pane| pane.view_name.as_deref() == Some(tab_name))
                .collect())
        },
        |panes: &Vec<PaneRef>| panes.len() == want,
        &format!("{want} terminal panes in {session}/{tab_name}"),
    )
}

/// Poll until the session's first tab is `expected`, or time out.
fn wait_for_first_tab(session: &LiveZellijSession, expected: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tab_names_in_order(session).first().map(String::as_str) == Some(expected) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Poll `query-tab-names` until a tab named `tab_name` appears, or time out.
fn wait_for_tab_named(session: &LiveZellijSession, tab_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listed = session
            .command()
            .args(["--session", session.name(), "action", "query-tab-names"])
            .bounded_output();
        if let Ok(out) = listed
            && out.status.success()
            && String::from_utf8_lossy(&out.stdout)
                .lines()
                .any(|line| line.trim() == tab_name)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
