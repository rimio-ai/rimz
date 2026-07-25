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
            detected_view_size: None,
            rimz_bin: stub.to_path_buf(),
            pristine_birth: false,
            config: rimz::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        },
    }
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

    let opts = background_view_opts(&name, &stub);

    let first = backend.open_background_view(&opts).expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        wait_for_tab_named(&room, "rimzd"),
        "expected a rimzd tab after launch",
    );

    let second = backend.open_background_view(&opts).expect("second launch");
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
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
