use super::*;
use rimz::mux::{DaemonView, HostPane};

/// A `BackgroundViewOptions` for a session whose content and host panes are
/// long-lived `sleep` commands and whose sidebar runs the alive-keeping `stub`,
/// so the launched tab is a faithful `sidebar | content | host`.
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
        },
        sidebar: SidebarPaneOptions {
            session_name: session.to_owned(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgview")),
            project_root: std::env::temp_dir(),
            cwd: std::env::temp_dir(),
            birth_size: SidebarWidth::default().birth_size(Some(120)),
            rimz_bin: stub.to_path_buf(),
            replace_existing: false,
            config: rimz::config::MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        },
    }
}

/// `open_background_view` opens a dedicated, named tab born `sidebar | content |
/// host`, and is idempotent on that tab name: a second call launches nothing.
#[test]
fn open_background_view_creates_named_tab_idempotently() {
    require_zellij!();

    let name = unique_session_name("bgview");
    let session = ZellijSession::spawn(&name);
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let (_stub_dir, stub) = sidebar_command_stub();

    let opts = background_view_opts(&name, &stub);

    let first = backend.open_background_view(&opts).expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        wait_for_tab_named(session.xdg.path(), &name, "rimzd"),
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

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("bgfirst");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
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
    };
    ZellijBackend::with_runtime_dir(xdg.path())
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgfirst")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                birth_size: SidebarWidth::default().birth_size(Some(120)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            Some(&daemon),
        )
        .expect("open_sidebar with daemon");

    assert!(
        wait_for_tab_named(xdg.path(), &name, "rimzd"),
        "expected a rimzd tab after birth",
    );
    assert!(
        wait_for_first_tab(xdg.path(), &name, "rimzd"),
        "daemon tab must lead the session; saw {:?}",
        tab_names_in_order(xdg.path(), &name),
    );
    // Two tabs: the daemon tab and the working tab born beside it.
    assert_eq!(
        tab_names_in_order(xdg.path(), &name).len(),
        2,
        "birth layout should produce exactly the daemon + working tabs",
    );
}

/// `open_sidebar` also accepts a daemon view with no daemon hosts: `rimzd`
/// still leads and is born as the two-column `sidebar | content` tab.
#[test]
fn open_sidebar_with_stats_only_daemon_view_leads_with_two_columns() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("bgstats");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());

    let daemon = DaemonView {
        name: "rimzd".to_owned(),
        content: vec![HostPane {
            argv: vec!["sleep".to_owned(), "120".to_owned()],
            cwd: cwd.path().to_path_buf(),
        }],
        hosts: Vec::new(),
    };
    backend
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgstats")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                birth_size: SidebarWidth::default().birth_size(Some(120)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            Some(&daemon),
        )
        .expect("open_sidebar with content-only daemon");

    assert!(
        wait_for_first_tab(xdg.path(), &name, "rimzd"),
        "content-only daemon tab must lead the session; saw {:?}",
        tab_names_in_order(xdg.path(), &name),
    );
    let panes = wait_for_tab_pane_count(&backend, &name, "rimzd", 2);
    assert_eq!(
        panes.len(),
        2,
        "rimzd should be born sidebar | content with no daemon hosts: {panes:?}",
    );
}

/// `open_sidebar` stacks multiple configured content panes in the daemon
/// view's middle column even when no daemon hosts apply.
#[test]
fn open_sidebar_with_multi_content_daemon_view_stacks_content() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("bgcontent");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());

    let daemon = DaemonView {
        name: "rimzd".to_owned(),
        content: vec![
            HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: cwd.path().to_path_buf(),
            },
            HostPane {
                argv: vec!["sleep".to_owned(), "120".to_owned()],
                cwd: cwd.path().to_path_buf(),
            },
        ],
        hosts: Vec::new(),
    };
    backend
        .open_sidebar(
            &SidebarPaneOptions {
                session_name: name.clone(),
                workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-bgcontent")),
                project_root: cwd.path().to_path_buf(),
                cwd: cwd.path().to_path_buf(),
                birth_size: SidebarWidth::default().birth_size(Some(120)),
                rimz_bin: stub,
                replace_existing: false,
                config: rimz::config::MultiplexerConfig::default(),
                resume_tabs: Vec::new(),
                refresh_ms: None,
            },
            Some(&daemon),
        )
        .expect("open_sidebar with multi-content daemon");

    assert!(
        wait_for_first_tab(xdg.path(), &name, "rimzd"),
        "multi-content daemon tab must lead the session; saw {:?}",
        tab_names_in_order(xdg.path(), &name),
    );
    let panes = wait_for_tab_pane_count(&backend, &name, "rimzd", 3);
    assert_eq!(
        panes.len(),
        3,
        "rimzd should be born sidebar | stacked content with no daemon hosts: {panes:?}",
    );
}

/// The session's tab names in tab order (`query-tab-names` prints one per line).
fn tab_names_in_order(xdg: &Path, session: &str) -> Vec<String> {
    let out = scoped_zellij(xdg)
        .args(["--session", session, "action", "query-tab-names"])
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
fn wait_for_tab_pane_count(
    backend: &ZellijBackend,
    session: &str,
    tab_name: &str,
    want: usize,
) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last = Vec::new();
    loop {
        if let Ok(listing) = backend.list_panes(PaneListOptions {
            session_name: Some(session.to_owned()),
            ..Default::default()
        }) {
            last = listing
                .panes
                .into_iter()
                .filter(|pane| pane.view_name.as_deref() == Some(tab_name))
                .collect();
            if last.len() == want {
                return last;
            }
        }
        if Instant::now() >= deadline {
            return last;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Poll until the session's first tab is `expected`, or time out.
fn wait_for_first_tab(xdg: &Path, session: &str, expected: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if tab_names_in_order(xdg, session).first().map(String::as_str) == Some(expected) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Poll `query-tab-names` until a tab named `tab_name` appears, or time out.
fn wait_for_tab_named(xdg: &Path, session: &str, tab_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listed = scoped_zellij(xdg)
            .args(["--session", session, "action", "query-tab-names"])
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
