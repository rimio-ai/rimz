#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

#[test]
fn sidebar_reload_keeps_mouse_capture_alive() {
    require_tmux!();
    let env = Env::new();
    let server = TmuxServer::in_runtime_root(&env.runtime_root);
    let session = "mouse-reload";
    let binary_dir = TempDir::new().expect("binary tempdir");
    let binary = binary_dir.path().join("rimz");
    std::fs::copy(env.rimz_bin(), &binary).expect("copy rimz binary");

    server
        .backend
        .ensure_session(&session_opts(
            session,
            env.workspace_id.clone(),
            &env.project_root,
            &env.project_root,
            Some((120, 40)),
        ))
        .expect("ensure_session");
    for (name, value) in [
        ("XDG_STATE_HOME", env.state_root()),
        ("XDG_RUNTIME_DIR", env.runtime_root.clone()),
        ("XDG_CONFIG_HOME", env.config_root()),
        ("HOME", env.home_root.clone()),
        ("RIMZ_BIN", binary.clone()),
    ] {
        server.tmux(&[
            "set-environment",
            "-t",
            session,
            name,
            value.to_str().expect("utf8 test path"),
        ]);
    }
    let opts = SidebarPaneOptions {
        workspace_id: env.workspace_id.clone(),
        project_root: env.project_root.clone(),
        cwd: env.project_root.clone(),
        ..sidebar_opts(session, binary.clone(), Some(120))
    };
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open sidebar");
    let pane = wait_for_sidebar_pane(&server, session, None);
    wait_for_mouse_capture(&server, &pane);
    assert_click_wheel_tracking_only(&server, &pane);
    let heartbeat = wait_for_sidebar_heartbeat(&env);
    let startup_seen = rimz::sidebar::heartbeat::SidebarHeartbeat::read_from(&heartbeat)
        .expect("read initial sidebar heartbeat")
        .last_seen;
    // Let the startup maintenance heartbeat land before taking the baseline.
    // The old worker then cannot publish another heartbeat during the reload,
    // so a newer timestamp confirms that the replacement worker is live.
    let initial_seen = wait_for_heartbeat_after(&heartbeat, startup_seen);

    let replacement = binary.with_extension("new");
    std::fs::copy(&binary, &replacement).expect("stage replacement binary");
    let mut staged = std::fs::OpenOptions::new()
        .append(true)
        .open(&replacement)
        .expect("open replacement binary");
    std::io::Write::write_all(&mut staged, &[0]).expect("change replacement bytes");
    drop(staged);
    std::fs::rename(&replacement, &binary).expect("install replacement binary");

    server.tmux(&["send-keys", "-t", pane.raw(), "r"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let handoff_started = Instant::now();
    loop {
        let mouse = server.display(pane.raw(), "#{mouse_any_flag}");
        if mouse != "1" {
            let panes = list_session_panes(&server, session);
            let current_heartbeat =
                rimz::sidebar::heartbeat::SidebarHeartbeat::read_from(&heartbeat).ok();
            panic!(
                "mouse capture dropped {:?} into reload; panes={panes:?}; heartbeat={current_heartbeat:?}",
                handoff_started.elapsed(),
            );
        }
        if rimz::sidebar::heartbeat::SidebarHeartbeat::read_from(&heartbeat)
            .is_ok_and(|heartbeat| heartbeat.last_seen > initial_seen)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reloaded sidebar did not publish a fresh heartbeat",
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_click_wheel_tracking_only(&server, &pane);
}

fn wait_for_mouse_capture(server: &TmuxServer, pane: &PaneId) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if server.display(pane.raw(), "#{mouse_any_flag}") == "1" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sidebar did not enable mouse capture",
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn assert_click_wheel_tracking_only(server: &TmuxServer, pane: &PaneId) {
    assert_eq!(server.display(pane.raw(), "#{mouse_standard_flag}"), "1");
    assert_eq!(server.display(pane.raw(), "#{mouse_sgr_flag}"), "1");
    assert_eq!(
        server.display(pane.raw(), "#{mouse_all_flag}"),
        "0",
        "all-motion tracking re-churns the outer terminal",
    );
    assert_eq!(server.display(pane.raw(), "#{mouse_button_flag}"), "0");
}

fn wait_for_sidebar_heartbeat(env: &Env) -> PathBuf {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(entries) = std::fs::read_dir(env.heartbeat_dir())
            && let Some(path) = entries
                .flatten()
                .map(|entry| entry.path())
                .find(|path| rimz::sidebar::heartbeat::SidebarHeartbeat::is_heartbeat_file(path))
        {
            return path;
        }
        assert!(
            Instant::now() < deadline,
            "sidebar did not publish its initial heartbeat",
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_heartbeat_after(path: &Path, prior: jiff::Timestamp) -> jiff::Timestamp {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(heartbeat) = rimz::sidebar::heartbeat::SidebarHeartbeat::read_from(path)
            && heartbeat.last_seen > prior
        {
            return heartbeat.last_seen;
        }
        assert!(
            Instant::now() < deadline,
            "sidebar heartbeat did not advance",
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn sidebar_width_step_is_exact_two_columns() {
    require_tmux!();
    let session = "width-step";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    let (_stub_dir, stub) = sidebar_command_stub();
    server
        .backend
        .open_sidebar(&sidebar_opts(session, stub, Some(120)), None)
        .expect("open sidebar");
    let pane = wait_for_sidebar_pane(&server, session, None);
    let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-width-step"));
    let runtime = RuntimePaths::under(workspace_id, server._tempdir.path()).expect("runtime");
    let step = server
        .backend
        .sidebar_width_step(&runtime, session, &pane, None)
        .expect("read sidebar step");
    assert_eq!(step.cols, 2);
    assert!(step.exact);
}

#[test]
fn sidebar_widths_converge_per_window_and_refresh_future_births() {
    require_tmux!();
    let server = TmuxServer::new();
    ensure_rimz_session(&server, "verdict", Some((120, 50)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts("verdict", stub, Some(120));
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    // The terminal "grows": windows born from now on adopt 340 columns. The
    // existing hook option still carries the 120-column birth target until a
    // width convergence observes the new live view.
    server.tmux(&["set-option", "-t", "verdict", "default-size", "340x50"]);
    server.tmux(&["new-window", "-t", "verdict"]);
    assert_eq!(
        server.display("verdict:1", "#{window_width}"),
        "340",
        "the new window adopts the grown geometry",
    );
    assert_eq!(
        left_pane_width(&server, "verdict:1"),
        Some(30),
        "the pre-convergence hook still carries the 30-column launch target",
    );
    let pane = left_pane_id(&server, "verdict:1").expect("left pane id");
    server
        .backend
        .nudge_sidebar_width("verdict", &pane, 30, 72)
        .expect("nudge live width");
    assert_eq!(left_pane_width(&server, "verdict:1"), Some(72));
    server
        .backend
        .record_sidebar_width_default("verdict", 72)
        .expect("record future width");
    server.tmux(&["new-window", "-t", "verdict"]);
    assert_eq!(
        left_pane_width(&server, "verdict:2"),
        Some(72),
        "the refreshed hook births the next wide window at the live cap",
    );

    for window in 0..=2 {
        let target = format!("verdict:{window}");
        let pane = left_pane_id(&server, &target).expect("left pane id");
        let current = left_pane_width(&server, &target).expect("left pane width") as u16;
        server
            .backend
            .nudge_sidebar_width("verdict", &pane, current, 55)
            .expect("nudge override width");
        assert_eq!(left_pane_width(&server, &target), Some(55));
    }
    server
        .backend
        .record_sidebar_width_default("verdict", 55)
        .expect("record override width");
    server.tmux(&["new-window", "-t", "verdict"]);
    assert_eq!(
        left_pane_width(&server, "verdict:3"),
        Some(55),
        "future windows inherit the refreshed absolute override",
    );
    let _client = AttachedTmuxClient::attach(&server.socket, "verdict", 340, 50);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !server.client_widths("verdict").contains(&340) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        server.client_widths("verdict").contains(&340),
        "the wide client must register before per-window reconcile",
    );
    server.tmux(&["resize-window", "-t", "verdict:0", "-x", "120", "-y", "50"]);
    assert_eq!(server.display("verdict:0", "#{window_width}"), "120");
    for window in 1..=3 {
        assert_eq!(
            server.display(&format!("verdict:{window}"), "#{window_width}"),
            "340",
            "the fixture must retain a wide view for window {window}",
        );
    }

    let mut reload_opts = opts;
    reload_opts.target = rimz::mux::SidebarTarget {
        share: rimz::mux::WidthPermille::from_cols(
            std::num::NonZeroU16::new(55).expect("nonzero test width"),
            std::num::NonZeroU16::new(340).expect("nonzero test view"),
        ),
        pinned: true,
        ..reload_opts.target
    };
    let liveness = rimz::mux::SidebarLiveness {
        claimed_panes: (0..=3)
            .map(|window| {
                left_pane_id(&server, &format!("verdict:{window}")).expect("left sidebar pane")
            })
            .collect(),
        ..Default::default()
    };
    server
        .backend
        .reconcile_sidebars(&reload_opts, &liveness)
        .expect("reconcile_sidebars");
    assert_eq!(
        left_pane_width(&server, "verdict:0"),
        Some(24),
        "the pinned share floors at the minimum on the original 120-column window",
    );
    for window in 1..=3 {
        assert_eq!(
            left_pane_width(&server, &format!("verdict:{window}")),
            Some(55),
            "the pinned share renders independently on 340-column window {window}",
        );
    }
}

#[test]
fn sidebar_birth_and_first_attach_preserve_work_shell_contract() {
    require_tmux!();
    let session = "rimz-pristine-birth";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((100, 30)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let mut opts = sidebar_opts(session, stub, Some(100));
    opts.pristine_birth = true;
    let sidebar_cols = u64::from(opts.target.cols(Some(100)).get());
    let birth_shell_cols = 100 - sidebar_cols - 1;
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    server.wait_for_pane_command(session, "rimz-sidebar");
    let panes = list_session_panes(&server, session);
    assert_eq!(
        panes.len(),
        2,
        "birth leaves sidebar and work shell: {panes:?}"
    );
    let sidebar = panes
        .iter()
        .find(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
        .expect("sidebar pane");
    let work = panes
        .iter()
        .find(|pane| pane.pane_id != sidebar.pane_id)
        .expect("work pane");
    let geoms = server.wait_for_panes(session, 2);
    let sidebar_geom = geoms
        .iter()
        .find(|pane| pane.id == sidebar.pane_id.raw())
        .expect("sidebar geometry");
    let work_geom = geoms
        .iter()
        .find(|pane| pane.id == work.pane_id.raw())
        .expect("work geometry");
    assert_eq!(sidebar_geom.left, 0, "sidebar is leftmost: {geoms:?}");
    assert_eq!(
        sidebar_geom.width, sidebar_cols,
        "sidebar keeps the birth verdict width: {geoms:?}",
    );
    assert_eq!(
        work_geom.left,
        sidebar_cols + 1,
        "work shell starts right of the sidebar border: {geoms:?}",
    );
    assert_eq!(
        work_geom.width, birth_shell_cols,
        "work shell is born at the detached width: {geoms:?}",
    );
    assert_eq!(
        server.display(session, "#{pane_id}"),
        work.pane_id.raw(),
        "session focus lands on the work shell",
    );
    let work_id = work.pane_id.raw().to_owned();
    let birth_pid = server.display(&work_id, "#{pane_pid}");
    let _watch = rimz::mux::tmux::PresenceWatch::attach(&server.socket, session)
        .expect("attach control watch");
    server.wait_for_control_client(session);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        server.display(&work_id, "#{pane_pid}"),
        birth_pid,
        "control client must leave first-human-attach cleanup armed",
    );
    let _client = AttachedTmuxClient::attach(&server.socket, session, 120, 30);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.display(&work_id, "#{pane_pid}") == birth_pid && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_ne!(
        server.display(&work_id, "#{pane_pid}"),
        birth_pid,
        "first human attach respawns the birth work shell",
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let (sidebar_geom, work_geom, final_width) = loop {
        let attached = server.wait_for_panes(session, 2);
        let sidebar = attached
            .iter()
            .find(|pane| pane.id == sidebar.pane_id.raw())
            .expect("attached sidebar geometry");
        let work = attached
            .iter()
            .find(|pane| pane.id == work_id)
            .expect("attached work geometry");
        let width = server
            .display(session, "#{window_width}")
            .parse::<u64>()
            .expect("window width");
        if sidebar.width + work.width + 1 == width || Instant::now() >= deadline {
            break (sidebar.clone(), work.clone(), width);
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(sidebar_geom.left, 0);
    assert_eq!(work_geom.left, sidebar_geom.width + 1);
    assert_eq!(sidebar_geom.width + work_geom.width + 1, final_width);
    assert_eq!(server.display(session, "#{pane_id}"), work_id);
}

#[test]
fn new_window_hook_respawns_plain_shell_at_final_width_only() {
    require_tmux!();
    let session = "rimz-hook-plain-shell";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((100, 30)));
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(session, stub, Some(100));
    let sidebar_cols = u64::from(opts.target.cols(Some(100)).get());
    server
        .backend
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    server.wait_for_pane_command(session, "rimz-sidebar");
    server.tmux(&["new-window", "-d", "-t", session, "-n", "plain"]);
    let plain_target = format!("{session}:plain");
    let plain_window = server.display(&plain_target, "#{window_id}");
    let plain_width = server
        .display(&plain_target, "#{window_width}")
        .parse::<u64>()
        .expect("plain window width");
    let plain_geoms = server.wait_for_panes(&plain_target, 2);
    let plain_panes = wait_for_hook_docked_window_panes(&server, session, &plain_window);
    assert_eq!(
        plain_panes.len(),
        2,
        "plain window should hold sidebar and work shell: {plain_panes:?}",
    );
    let plain_sidebar = plain_panes
        .iter()
        .find(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
        .expect("plain sidebar pane");
    let plain_work = plain_panes
        .iter()
        .find(|pane| pane.pane_id != plain_sidebar.pane_id)
        .expect("plain work pane");
    let plain_sidebar_geom = plain_geoms
        .iter()
        .find(|pane| pane.id == plain_sidebar.pane_id.raw())
        .expect("plain sidebar geometry");
    let plain_work_geom = plain_geoms
        .iter()
        .find(|pane| pane.id == plain_work.pane_id.raw())
        .expect("plain work geometry");
    assert_eq!(plain_sidebar_geom.left, 0, "sidebar is leftmost");
    assert_eq!(
        plain_sidebar_geom.width, sidebar_cols,
        "sidebar keeps the hook's birth width",
    );
    assert_eq!(
        plain_work_geom.left,
        sidebar_cols + 1,
        "work shell starts right of the sidebar border",
    );
    assert_eq!(
        plain_work_geom.width,
        plain_width - sidebar_cols - 1,
        "plain work shell is born at final width",
    );
    assert_eq!(
        server.display(&plain_target, "#{pane_id}"),
        plain_work.pane_id.raw(),
        "plain window focus lands on the work shell",
    );
    assert_eq!(
        server.display(plain_work.pane_id.raw(), "#{pane_start_command}"),
        rimz::harness::launch::user_shell_program(),
        "empty-start-command tabs are respawned as the user's shell",
    );
    let explicit_command = "cat";
    server.tmux(&[
        "new-window",
        "-d",
        "-t",
        session,
        "-n",
        "explicit",
        explicit_command,
    ]);
    let explicit_target = format!("{session}:explicit");
    let explicit_window = server.display(&explicit_target, "#{window_id}");
    let explicit_width = server
        .display(&explicit_target, "#{window_width}")
        .parse::<u64>()
        .expect("explicit window width");
    let explicit_geoms = server.wait_for_panes(&explicit_target, 2);
    let explicit_panes = wait_for_hook_docked_window_panes(&server, session, &explicit_window);
    assert_eq!(
        explicit_panes.len(),
        2,
        "explicit-command window should hold sidebar and work pane: {explicit_panes:?}",
    );
    let explicit_sidebar = explicit_panes
        .iter()
        .find(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
        .expect("explicit sidebar pane");
    let explicit_work = explicit_panes
        .iter()
        .find(|pane| pane.pane_id != explicit_sidebar.pane_id)
        .expect("explicit work pane");
    let explicit_sidebar_geom = explicit_geoms
        .iter()
        .find(|pane| pane.id == explicit_sidebar.pane_id.raw())
        .expect("explicit sidebar geometry");
    let explicit_work_geom = explicit_geoms
        .iter()
        .find(|pane| pane.id == explicit_work.pane_id.raw())
        .expect("explicit work geometry");
    assert_eq!(explicit_sidebar_geom.left, 0, "sidebar is leftmost");
    assert_eq!(
        explicit_sidebar_geom.width, sidebar_cols,
        "explicit-command sidebar keeps the hook's birth width",
    );
    assert_eq!(
        explicit_work_geom.left,
        sidebar_cols + 1,
        "explicit command starts right of the sidebar border",
    );
    assert_eq!(
        explicit_work_geom.width,
        explicit_width - sidebar_cols - 1,
        "explicit-command work pane keeps final width",
    );
    assert_eq!(
        server.display(explicit_work.pane_id.raw(), "#{pane_start_command}"),
        explicit_command,
        "explicit-command tabs keep their original process",
    );
}

#[test]
fn fresh_foreign_producer_still_repairs_tmux_session_view() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-foreign");
    server.tmux(&["rename-window", "-t", "rimz-foreign:0", "work"]);
    let workspace = TempDir::new().expect("workspace");
    let workspace_id = WorkspaceId::from_project_root(workspace.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), workspace.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_heartbeat(
        &runtime,
        workspace_id.clone(),
        &SidebarInstanceId::new(),
        MuxName::Zellij,
        "prior-zellij",
        &runtime.sock_dir.join("foreign.sock"),
        Some(PaneId::from_parts(MuxName::Zellij, "terminal_7")),
    )
    .expect("foreign heartbeat");
    assert!(rimz::sidebar::fresh_sidebar_present(&runtime));
    assert!(
        !server.has_after_new_window_hook("rimz-foreign"),
        "fixture starts with no session hook"
    );
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        workspace_id: workspace_id.clone(),
        project_root: workspace.path().to_path_buf(),
        cwd: workspace.path().to_path_buf(),
        ..sidebar_opts("rimz-foreign", stub, Some(80))
    };
    let outcome = launch_sidebar_if_needed(&server.backend, &runtime, &opts, None);
    assert_eq!(outcome, SidebarLaunchOutcome::SkippedFresh);
    server.wait_for_pane_command("rimz-foreign", "rimz-sidebar");
    assert!(
        server.has_after_new_window_hook("rimz-foreign"),
        "skipping producer launch should still install the tmux hook"
    );
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-foreign".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    assert_eq!(
        panes
            .iter()
            .filter(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
            .count(),
        1,
        "the working window should gain exactly one sidebar: {panes:?}",
    );
    assert!(
        panes.iter().any(|pane| {
            pane.view_name.as_deref() == Some("work")
                && pane.command.as_deref() == Some("rimz-sidebar")
        }),
        "the sidebar should be in the working window: {panes:?}",
    );
    server.tmux(&["set-hook", "-u", "-t", "rimz-foreign", "after-new-window"]);
    assert_eq!(
        launch_sidebar_if_needed(&server.backend, &runtime, &opts, None),
        SidebarLaunchOutcome::SkippedFresh,
    );
    server.tmux(&["new-window", "-t", "rimz-foreign"]);
    let window = server.display("rimz-foreign", "#{window_id}");
    let repaired = wait_for_hook_docked_window_panes(&server, "rimz-foreign", &window);
    assert_eq!(
        repaired
            .iter()
            .filter(|pane| pane.command.as_deref() == Some("rimz-sidebar"))
            .count(),
        1,
        "repaired hook docks one sidebar in the next window: {repaired:?}",
    );
}

/// The width of the left (`pane_left == 0`) pane in `target`, polling until
/// the window holds a second, hook-docked pane or the budget elapses.
fn left_pane_width(server: &TmuxServer, target: &str) -> Option<u64> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let stdout = server.stdout(&[
            "list-panes",
            "-t",
            target,
            "-F",
            "#{pane_left}:#{pane_width}",
        ]);
        let panes: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
        if panes.len() >= 2
            && let Some(width) = panes.iter().find_map(|line| {
                let (left, width) = line.split_once(':')?;
                (left == "0").then(|| width.parse().ok()).flatten()
            })
        {
            return Some(width);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn left_pane_id(server: &TmuxServer, target: &str) -> Option<rimz::ids::PaneId> {
    let stdout = server.stdout(&["list-panes", "-t", target, "-F", "#{pane_left}:#{pane_id}"]);
    stdout.lines().find_map(|line| {
        let (left, id) = line.split_once(':')?;
        (left == "0").then(|| rimz::ids::PaneId::from_parts(rimz::MuxName::Tmux, id))
    })
}

#[test]
fn open_sidebar_seeds_resume_windows_idempotently() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-resume");
    let (_stub_dir, stub) = sidebar_command_stub();
    let sidebar = SidebarPaneOptions {
        // A harmless stand-in for the agent CLIs (`claude`/`codex` aren't on a CI
        // PATH); the seeding contract is the window, not what runs in it.
        resume_tabs: vec![rimz::mux::ResumeTab {
            label: "#feature".to_owned(),
            cwd: std::env::temp_dir(),
            layout: rimz::mux::LayoutPanes {
                columns: vec![
                    rimz::mux::LayoutColumn {
                        panes: vec![rimz::mux::PaneCmd {
                            argv: vec!["sleep".to_owned(), "120".to_owned()],
                            name: None,
                        }],
                        stacked: false,
                    },
                    rimz::mux::LayoutColumn {
                        panes: vec![
                            rimz::mux::PaneCmd {
                                argv: vec!["sleep".to_owned(), "120".to_owned()],
                                name: None,
                            },
                            rimz::mux::PaneCmd {
                                argv: vec!["sleep".to_owned(), "120".to_owned()],
                                name: None,
                            },
                        ],
                        stacked: false,
                    },
                ],
            },
        }],
        ..sidebar_opts("rimz-resume", stub, Some(80))
    };
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("open_sidebar");
    assert!(
        server
            .window_names("rimz-resume")
            .iter()
            .any(|name| name == "#feature"),
        "expected a resumed channel window, got {:?}",
        server.window_names("rimz-resume"),
    );
    // Born `sidebar | agents…`: the hook-docked sidebar beside the agent panes.
    let agent_panes = server
        .backend
        .list_panes(rimz::mux::PaneListOptions {
            session_name: Some("rimz-resume".to_owned()),
            ..Default::default()
        })
        .expect("list panes")
        .panes
        .into_iter()
        .filter(|pane| pane.view_name.as_deref() == Some("#feature"))
        .count();
    assert_eq!(
        agent_panes, 4,
        "resumed window should be born sidebar | agents"
    );
    let panes = server.wait_for_panes("rimz-resume:#feature", 4);
    let work = panes
        .iter()
        .filter(|pane| pane.left > 0)
        .collect::<Vec<_>>();
    assert_eq!(work.len(), 3, "expected three work panes: {panes:?}");
    let left_column = work.iter().map(|pane| pane.left).min().expect("work pane");
    let right_column = work.iter().map(|pane| pane.left).max().expect("work pane");
    assert_ne!(
        left_column, right_column,
        "team restore should create two work columns: {panes:?}"
    );
    assert_eq!(
        work.iter().filter(|pane| pane.left == left_column).count(),
        1,
        "planner column stays full-height on the left: {panes:?}"
    );
    assert_eq!(
        work.iter().filter(|pane| pane.left == right_column).count(),
        2,
        "coder/reviewer column stays stacked on the right: {panes:?}"
    );
    assert_eq!(
        left_pane_width(&server, "rimz-resume:#feature"),
        Some(u64::from(
            sidebar
                .target
                .cols(sidebar.detected_view_size.map(|(cols, _)| cols))
                .get(),
        )),
        "resume seeding keeps the hook-docked sidebar at the birth width"
    );
    // A re-run finds the window already present and seeds nothing new.
    server
        .backend
        .open_sidebar(&sidebar, None)
        .expect("second open_sidebar");
    let resumed = server
        .window_names("rimz-resume")
        .into_iter()
        .filter(|name| name == "#feature")
        .count();
    assert_eq!(
        resumed, 1,
        "resume seeding is idempotent on the window name"
    );
}
