//! Live Zellij backend tests for the M0b spike.
//!
//! Each test spawns a real `zellij` session in a portable-pty and runs the
//! `ZellijBackend` against it. The whole file becomes a no-op (early-return
//! per test, message printed once) when the `zellij` binary is not on PATH.
//! The trace-shim wakeup-walk test that verifies the broadcast `zellij
//! pipe` invocation lives in a separate file (`wakeup_pipe.rs`) so its env
//! mutation does not race with these tests.

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use fs4::FileExt;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use rimz::feed::PaneRef;
use rimz::ids::{MuxName, WorkspaceId};
use rimz::mux::{MuxBackend, PaneListOptions, SidebarPaneOptions, ZellijBackend, zellij};
use tempfile::TempDir;

const SPAWN_TIMEOUT: Duration = Duration::from_secs(30);

/// Skip the test (return) if the host has no `zellij` binary on PATH.
macro_rules! require_zellij {
    () => {
        if which::which("zellij").is_err() {
            eprintln!("zellij not on PATH; skipping test");
            return;
        }
    };
}

/// Owns a live Zellij session for the duration of one test. Spawned via a
/// portable-pty so the child has the terminal it expects; the master is
/// kept alive (and silently drained) to avoid SIGHUP'ing the session.
struct ZellijSession {
    name: String,
    _serial: ZellijTestGuard,
    _master: Box<dyn portable_pty::MasterPty + Send>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_thread: Option<std::thread::JoinHandle<()>>,
}

impl ZellijSession {
    fn spawn(name: impl Into<String>) -> Self {
        let serial = zellij_test_lock()
            .lock()
            .expect("zellij test lock poisoned");
        let name = name.into();
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("zellij");
        cmd.args(["attach", "--create", &name]);
        let child = pair.slave.spawn_command(cmd).expect("spawn zellij");
        drop(pair.slave);

        // Drain the PTY in the background so the kernel buffer never fills
        // and stalls the child. We do not parse anything; the channel of
        // record is the `zellij action ...` round-trip.
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let reader_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => continue,
                }
            }
        });

        let session = Self {
            name,
            _serial: serial,
            _master: pair.master,
            _child: child,
            _reader_thread: Some(reader_thread),
        };
        wait_until_session_listed(&session.name);
        session
    }
}

struct ZellijTestLock;

struct ZellijTestGuard {
    file: File,
}

impl ZellijTestLock {
    fn lock(&self) -> std::io::Result<ZellijTestGuard> {
        let path = std::env::temp_dir().join("rimz-zellij-integration.lock");
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.lock()?;
        Ok(ZellijTestGuard { file })
    }
}

impl Drop for ZellijTestGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn zellij_test_lock() -> &'static ZellijTestLock {
    static LOCK: OnceLock<ZellijTestLock> = OnceLock::new();
    LOCK.get_or_init(|| ZellijTestLock)
}

impl Drop for ZellijSession {
    fn drop(&mut self) {
        let _ = std::process::Command::new("zellij")
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

struct ZellijSessionCleanup {
    name: String,
}

impl ZellijSessionCleanup {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl Drop for ZellijSessionCleanup {
    fn drop(&mut self) {
        let _ = std::process::Command::new("zellij")
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

/// Poll `zellij list-sessions` until our name appears. Sessions take
/// 300–800 ms to register on a quiet host; we give it 30 s for slow
/// CI machines. We grep with `contains` because the line is wrapped in
/// ANSI color codes on Zellij 0.41+.
fn wait_until_session_listed(name: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            panic!("zellij session {name} never appeared in list-sessions");
        }
        let listed = std::process::Command::new("zellij")
            .arg("list-sessions")
            .output();
        if let Ok(out) = listed
            && out.status.success()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains(name) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn unique_session_name(prefix: &str) -> String {
    let id = uuid::Uuid::now_v7().simple().to_string();
    format!("rimz-{prefix}-{}", &id[..12])
}

fn sidebar_command_stub() -> (TempDir, PathBuf) {
    let dir = TempDir::new().expect("stub dir");
    let path = dir.path().join("rimz-stub");
    std::fs::write(&path, "#!/bin/sh\nsleep 30\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod");
    }
    (dir, path)
}

fn held_sidebar_layout(stub: &Path) -> String {
    let stub = serde_json::to_string(&stub.to_string_lossy()).expect("kdl escape");
    format!(
        r#"layout {{
    pane split_direction="vertical" {{
        pane command={stub} name="rimz-sidebar" size="30%" {{
            args "sidebar" "serve" "--mux" "zellij"
            start_suspended true
        }}
        pane focus=true
    }}
}}
"#,
    )
}

/// Sanity: spawn a session, see it in `list_sessions`. Establishes that the
/// portable-pty harness can reach a usable Zellij.
#[test]
fn ensure_and_list_sessions_round_trip() {
    require_zellij!();

    let name = unique_session_name("list");
    let _session = ZellijSession::spawn(&name);

    let listed = ZellijBackend
        .list_sessions()
        .expect("list_sessions succeeds against a live zellij");
    assert!(
        listed.iter().any(|s| s == &name),
        "expected session {name} in {listed:?}",
    );
}

/// `open_sidebar` births the session from a layout: a left ~30% native
/// `rimz-sidebar` pane plus a focused terminal pane. No pre-create, no WASM
/// plugin, no post-creation move/resize.
#[test]
fn open_sidebar_creates_native_pane() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("sidebar");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-test")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");

    let panes = wait_for_pane_count(&name, 2);
    assert!(
        panes.len() >= 2,
        "layout should create a sidebar + terminal pane in {name}: {panes:?}",
    );
    assert_sidebar_is_left_thirty_percent(&name);
    assert_session_has_bottom_bar(&name);
}

/// `dump-layout` exposes the template Zellij will use for future user-created
/// tabs. It must contain the explicit focused terminal, not just the sidebar
/// split; otherwise existing attached sessions create sidebar-only tabs.
#[test]
fn open_sidebar_installs_a_right_terminal_in_the_new_tab_template() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("template");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-template")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");
    wait_for_pane_count(&name, 2);

    let template = new_tab_template_dump(&name);
    assert!(
        template.contains("rimz-sidebar"),
        "new tab template should carry the sidebar pane:\n{template}",
    );
    assert!(
        template.contains("pane focus=true"),
        "new tab template should carry an explicit focused right terminal:\n{template}",
    );
}

/// Zellij treats command panes as held until the user presses Enter unless the
/// layout opts out. Rimz sidebars must start immediately in attached and
/// background-created sessions.
#[test]
fn open_sidebar_starts_sidebar_without_a_run_prompt() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("runprompt");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-runprompt")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");
    wait_for_pane_count(&name, 2);

    assert_sidebars_not_held(&name, "initial tab");

    open_new_tab(&name);
    wait_for_tab_count(&name, 2);
    assert_sidebars_not_held(&name, "new tab");
}

/// The sidebar layout replaces Zellij's default tab template, so it must re-add
/// the bottom bar plugin itself. Assert the born session actually carries it —
/// not just that the layout string mentions it.
fn assert_session_has_bottom_bar(session: &str) {
    let output = std::process::Command::new("zellij")
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .output()
        .expect("list-panes for bar check");
    assert!(output.status.success(), "list-panes for bar check failed");
    let panes: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list-panes bar json");
    let has_bar = panes.as_array().expect("pane array").iter().any(|pane| {
        pane.get("is_plugin").and_then(|v| v.as_bool()) == Some(true)
            && pane
                .get("title")
                .and_then(|v| v.as_str())
                .is_some_and(|title| title.contains("compact-bar"))
    });
    assert!(
        has_bar,
        "session {session} should carry a bottom bar plugin: {panes:?}"
    );
}

/// Re-running `open_sidebar` against a *live* session takes the no-op arm of the
/// session-state branch: it neither errors nor injects a second sidebar, and the
/// 30% layout is preserved. (The exited arm — delete then rebirth — cannot be
/// driven headlessly: an EXITED-resurrectable session requires a prior attach +
/// serialization. Its classifier is covered by the `session_state` unit test.)
#[test]
fn open_sidebar_on_live_session_is_idempotent() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("idem");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = SidebarPaneOptions {
        session_name: name.clone(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-idem")),
        cwd: cwd.path().to_path_buf(),
        width_percent: 30,
        rimz_bin: stub,
        replace_existing: false,
    };

    ZellijBackend
        .open_sidebar(&opts)
        .expect("first open_sidebar");
    let first = wait_for_pane_count(&name, 2);
    assert!(
        first.len() >= 2,
        "first birth should create a sidebar + terminal pane: {first:?}",
    );

    // Second call sees a live session and must leave it untouched.
    ZellijBackend
        .open_sidebar(&opts)
        .expect("second open_sidebar");
    let second = wait_for_pane_count(&name, 2);
    assert_eq!(
        second.len(),
        first.len(),
        "re-opening a live session must not add or drop panes: {second:?}",
    );
    assert_sidebar_is_left_thirty_percent(&name);
}

/// A session with a `rimz-sidebar` pane is still broken if that pane is held at
/// Zellij's "Waiting to run" prompt. Re-running `open_sidebar` must treat it
/// as unhealthy and rebirth the session from the current layout.
#[test]
fn open_sidebar_heals_a_live_session_with_a_held_sidebar() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("heldsidebar");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = cwd.path().join("held-sidebar.kdl");
    std::fs::write(&layout, held_sidebar_layout(&stub)).expect("write held layout");

    let output = std::process::Command::new("zellij")
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .output()
        .expect("spawn held-sidebar session");
    assert!(
        output.status.success(),
        "spawn held-sidebar session failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    wait_for_pane_count(&name, 2);
    assert!(
        session_has_held_sidebar(&name),
        "test setup should produce a held sidebar pane",
    );

    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-held-sidebar")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");
    wait_for_pane_count(&name, 2);

    assert_sidebars_not_held(&name, "recreated session");
    open_new_tab(&name);
    wait_for_tab_count(&name, 2);
    assert_sidebars_not_held(&name, "new tab after recreate");
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

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("nosb");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");

    // Birth a live session with a plain, sidebar-less layout. The pane runs a
    // long sleep so the unattached background session stays alive deterministically.
    let layout = cwd.path().join("plain.kdl");
    std::fs::write(
        &layout,
        "layout {\n    pane command=\"sleep\" {\n        args \"60\"\n    }\n}\n",
    )
    .expect("write plain layout");
    let created = std::process::Command::new("zellij")
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout)
        .status()
        .expect("create plain session");
    assert!(created.success(), "create-background failed for {name}");
    let plain = wait_for_pane_count(&name, 1);
    assert!(
        !plain.is_empty(),
        "plain session should have a pane before open_sidebar: {plain:?}",
    );

    // `open_sidebar` must heal it: tear the sidebar-less session down and
    // rebirth one that carries the sidebar.
    let (_stub_dir, stub) = sidebar_command_stub();
    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-sidebar-nosb")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");

    let healed = wait_for_pane_count(&name, 2);
    assert!(
        healed.len() >= 2,
        "open_sidebar should rebirth a sidebar-less live session with a sidebar: {healed:?}",
    );
    assert_sidebar_is_left_thirty_percent(&name);
}

/// Every tab born from the sidebar layout — the initial one *and* any the user
/// opens later — must carry a right terminal pane next to the sidebar.
/// Regression test for "a new tab in an existing session shows only the
/// sidebar, no right panel": the template must spell out the right pane instead
/// of relying on Zellij's `children` placeholder semantics.
#[test]
fn new_tab_is_born_with_a_right_terminal() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("newtabpane");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-newtab-pane")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");
    wait_for_pane_count(&name, 2);

    open_new_tab(&name);
    wait_for_tab_count(&name, 2);

    // Every tab must have a sidebar *and* at least one terminal beside it.
    for tab in tab_ids(&name) {
        let terminals = nonplugin_titles_in_tab(&name, tab);
        let has_sidebar = terminals.iter().any(|t| t == "rimz-sidebar");
        let has_terminal = terminals.iter().any(|t| t != "rimz-sidebar");
        assert!(
            has_sidebar && has_terminal,
            "tab {tab} should carry the sidebar and a right terminal, got {terminals:?}",
        );
    }
}

/// Focus must land on the right terminal, never the sidebar — on launch and on
/// every tab opened afterwards. Regression test for "on launch the focus is the
/// sidebar, not the right panel": with the old layout a tab born from the
/// template could strand focus on the sidebar even when Zellij materialized the
/// right pane.
#[test]
fn tabs_focus_the_terminal_not_the_sidebar() {
    require_zellij!();

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("focusterm");
    let _cleanup = ZellijSessionCleanup::new(&name);
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();

    ZellijBackend
        .open_sidebar(&SidebarPaneOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-focus-term")),
            cwd: cwd.path().to_path_buf(),
            width_percent: 30,
            rimz_bin: stub,
            replace_existing: false,
        })
        .expect("open_sidebar");
    wait_for_pane_count(&name, 2);

    open_new_tab(&name);
    wait_for_tab_count(&name, 2);

    // Each tab tracks its own focused pane; none may be the sidebar.
    for tab in tab_ids(&name) {
        let focused = focused_nonplugin_title_in_tab(&name, tab)
            .unwrap_or_else(|| panic!("tab {tab} has no focused terminal pane"));
        assert_ne!(
            focused, "rimz-sidebar",
            "tab {tab} focuses the sidebar; focus must land on the right terminal",
        );
    }
}

/// Open a second tab the way a user would, from the default tab template.
fn open_new_tab(session: &str) {
    let output = std::process::Command::new("zellij")
        .args(["--session", session, "action", "new-tab"])
        .output()
        .expect("new-tab");
    assert!(
        output.status.success(),
        "new-tab failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Parsed `list-panes -j -a` for `session`, or an empty array on any failure.
fn list_panes_json(session: &str) -> serde_json::Value {
    std::process::Command::new("zellij")
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice(&out.stdout).ok())
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
}

fn assert_sidebars_not_held(session: &str, context: &str) {
    let panes = list_panes_json(session);
    let sidebars: Vec<&serde_json::Value> = panes
        .as_array()
        .expect("pane array")
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .collect();
    assert!(
        !sidebars.is_empty(),
        "rimz-sidebar pane missing while checking {context}:\n{panes}",
    );
    for sidebar in sidebars {
        assert_ne!(
            sidebar.get("is_held").and_then(|value| value.as_bool()),
            Some(true),
            "sidebar command pane is waiting for Enter instead of running in {context}:\n{sidebar}",
        );
    }
}

fn session_has_held_sidebar(session: &str) -> bool {
    list_panes_json(session)
        .as_array()
        .map(|panes| {
            panes.iter().any(|pane| {
                pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                    && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
                    && pane.get("is_held").and_then(|value| value.as_bool()) == Some(true)
            })
        })
        .unwrap_or(false)
}

/// Dump just the `new_tab_template` section for readable assertions.
fn new_tab_template_dump(session: &str) -> String {
    let output = std::process::Command::new("zellij")
        .args(["--session", session, "action", "dump-layout"])
        .output()
        .expect("dump-layout");
    assert!(
        output.status.success(),
        "dump-layout failed for {session}: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let dump = String::from_utf8_lossy(&output.stdout);
    let start = dump
        .find("new_tab_template")
        .unwrap_or_else(|| panic!("dump-layout has no new_tab_template:\n{dump}"));
    dump[start..].to_owned()
}

/// Distinct tab ids that currently hold a non-plugin pane.
fn tab_ids(session: &str) -> Vec<u64> {
    let panes = list_panes_json(session);
    let mut ids: Vec<u64> = panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter_map(|p| p.get("tab_id").and_then(|v| v.as_u64()))
                .collect()
        })
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Titles of the non-plugin panes in `tab`.
fn nonplugin_titles_in_tab(session: &str, tab: u64) -> Vec<String> {
    let panes = list_panes_json(session);
    panes
        .as_array()
        .map(|panes| {
            panes
                .iter()
                .filter(|p| p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false))
                .filter(|p| p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab))
                .filter_map(|p| p.get("title").and_then(|v| v.as_str()).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Title of the focused non-plugin pane in `tab`, if any.
fn focused_nonplugin_title_in_tab(session: &str, tab: u64) -> Option<String> {
    let panes = list_panes_json(session);
    panes.as_array()?.iter().find_map(|p| {
        (p.get("is_plugin").and_then(|v| v.as_bool()) == Some(false)
            && p.get("tab_id").and_then(|v| v.as_u64()) == Some(tab)
            && p.get("is_focused").and_then(|v| v.as_bool()) == Some(true))
        .then(|| {
            p.get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        })
    })
}

/// Poll until at least `want` distinct tabs hold a non-plugin pane, or time out.
fn wait_for_tab_count(session: &str, want: usize) -> Vec<u64> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ids = tab_ids(session);
        if ids.len() >= want || Instant::now() >= deadline {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// End-to-end self-close: a real `rimz-sidebar` shares a tab with a terminal
/// pane that exits on its own. The sidebar polls `rimz pane list`, sees it is
/// alone, and exits; being `close_on_exit`, its pane then closes. We assert the
/// lone sidebar removes its own pane — the tab drops from two terminal panes to
/// zero. (Tearing down the now-empty tab/session is the multiplexer's job once
/// a client is attached; a never-attached background session lingers empty.)
#[test]
fn sidebar_self_closes_when_its_tab_empties() {
    require_zellij!();

    let rimz = assert_cmd::cargo::cargo_bin("rimz");
    let sidebar = assert_cmd::cargo::cargo_bin("rimz-sidebar");
    if !rimz.exists() || !sidebar.exists() {
        eprintln!("rimz/rimz-sidebar binaries not built; skipping self-close test");
        return;
    }

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("selfclose");
    let cwd = TempDir::new().expect("cwd tempdir");
    // One short XDG_RUNTIME_DIR for everything: zellij's *server* socket and
    // rimz's *wakeup* socket both live under it, so every zellij call touching
    // this session must share it — and it must stay short enough that the
    // workspace + 35-char instance id keep the socket under the 108-byte
    // AF_UNIX limit. A `prefix("rz")` tempdir buys that headroom.
    let xdg = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg tempdir");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    let layout = self_close_layout(&name, &rimz, &sidebar, xdg.path());
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");

    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout_path)
        .status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 2, Duration::from_secs(15)),
        "expected sidebar + terminal before self-close for {name}",
    );
    assert!(
        wait_for_nonplugin_panes(xdg.path(), &name, 0, Duration::from_secs(30)),
        "lone sidebar did not close its own pane after the terminal exited for {name}",
    );

    // On exit the sidebar removes its heartbeat (RuntimeFileGuard); otherwise it
    // stays mtime-fresh for the TTL and a later `rimz` launch skips relaunch,
    // rebirthing the session with no sidebar. Assert none lingers once gone.
    let heartbeat_dir = xdg
        .path()
        .join("rimz")
        .join("ws_0123456789abcdef01234567")
        .join("heartbeat");
    assert!(
        wait_for_no_sidebar_heartbeat(&heartbeat_dir, Duration::from_secs(5)),
        "sidebar heartbeat should be removed on self-close, found: {:?}",
        std::fs::read_dir(&heartbeat_dir)
            .map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
}

/// A tab born with only the sidebar has no useful work surface. The sidebar
/// must close itself even though it never saw a sibling pane first. Regression
/// test for a user-created second tab that showed only a full-width sidebar and
/// stayed open forever.
#[test]
fn sidebar_self_closes_when_its_tab_starts_empty() {
    require_zellij!();

    let rimz = assert_cmd::cargo::cargo_bin("rimz");
    let sidebar = assert_cmd::cargo::cargo_bin("rimz-sidebar");
    if !rimz.exists() || !sidebar.exists() {
        eprintln!("rimz/rimz-sidebar binaries not built; skipping sidebar-only test");
        return;
    }

    let _serial = zellij_test_lock()
        .lock()
        .expect("zellij test lock poisoned");
    let name = unique_session_name("emptytab");
    let cwd = TempDir::new().expect("cwd tempdir");
    let xdg = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg tempdir");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    let layout = sidebar_only_tab_layout(&name, &rimz, &sidebar, xdg.path());
    let layout_path = cwd.path().join("layout.kdl");
    std::fs::write(&layout_path, layout).expect("write layout");

    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name, "options"])
        .arg("--default-cwd")
        .arg(cwd.path())
        .arg("--default-layout")
        .arg(&layout_path)
        .status()
        .expect("create background session");
    assert!(created.success(), "create-background failed for {name}");

    assert!(
        wait_for_nonplugin_panes_in_tab(xdg.path(), &name, "main", 2, Duration::from_secs(15)),
        "expected main tab to keep sidebar + terminal for {name}",
    );
    assert!(
        wait_for_nonplugin_panes_in_tab(xdg.path(), &name, "orphan", 0, Duration::from_secs(10)),
        "sidebar-only orphan tab did not close its own pane for {name}",
    );
    assert!(
        wait_for_nonplugin_panes_in_tab(xdg.path(), &name, "main", 2, Duration::from_secs(2)),
        "orphan cleanup must not close the populated main tab for {name}",
    );
}

/// Poll until no `sidebar.*.json` heartbeat remains in `dir` (a missing dir
/// counts as none), or the timeout elapses.
fn wait_for_no_sidebar_heartbeat(dir: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let lingering = std::fs::read_dir(dir)
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("sidebar.") && n.ends_with(".json"))
                })
            })
            .unwrap_or(false);
        if !lingering {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Layout for the self-close test: a real `rimz-sidebar` renderer on the left
/// (env-scoped to a throwaway XDG dir) and a terminal that exits after a beat.
/// Both panes are `close_on_exit`, so each disappears when its command ends.
fn self_close_layout(session: &str, rimz: &Path, sidebar: &Path, xdg: &Path) -> String {
    let q = |s: String| serde_json::to_string(&s).expect("kdl escape");
    let serve = sidebar_serve_command(session, rimz, sidebar, xdg);
    format!(
        r#"layout {{
    default_tab_template split_direction="vertical" {{
        pane size="30%" name="rimz-sidebar" {{
            command "sh"
            args "-c" {serve}
            close_on_exit true
        }}
        children
    }}
    tab name="rimz" {{
        pane focus=true {{
            command "sleep"
            args "3"
            close_on_exit true
        }}
    }}
}}
"#,
        serve = q(serve),
    )
}

/// Layout with one healthy tab and one orphan sidebar-only tab. The healthy tab
/// proves the self-close decision is scoped to the sidebar's own tab.
fn sidebar_only_tab_layout(session: &str, rimz: &Path, sidebar: &Path, xdg: &Path) -> String {
    let q = |s: String| serde_json::to_string(&s).expect("kdl escape");
    let serve = sidebar_serve_command(session, rimz, sidebar, xdg);
    format!(
        r#"layout {{
    tab name="main" {{
        pane split_direction="vertical" {{
            pane size="30%" name="rimz-sidebar" {{
                command "sh"
                args "-c" {serve}
                close_on_exit true
            }}
            pane focus=true {{
                command "sleep"
                args "30"
                close_on_exit true
            }}
        }}
    }}
    tab name="orphan" {{
        pane name="rimz-sidebar" {{
            command "sh"
            args "-c" {serve}
            close_on_exit true
        }}
    }}
}}
"#,
        serve = q(serve),
    )
}

fn sidebar_serve_command(session: &str, rimz: &Path, sidebar: &Path, xdg: &Path) -> String {
    format!(
        "XDG_STATE_HOME={xdg} XDG_RUNTIME_DIR={xdg} RIMZ_BIN={rimz} \
         exec {sidebar} serve --mux zellij --workspace-id ws_0123456789abcdef01234567 \
         --session-name {session} --tick-seconds 1",
        xdg = xdg.display(),
        rimz = rimz.display(),
        sidebar = sidebar.display(),
    )
}

/// Poll `list_panes` until at least `want` panes appear (bounded). Returns the
/// last observation either way so the caller can assert and print it.
fn wait_for_pane_count(session: &str, want: usize) -> Vec<PaneRef> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let panes = ZellijBackend
            .list_panes(PaneListOptions {
                session_name: Some(session.to_owned()),
            })
            .unwrap_or_default();
        if panes.len() >= want || Instant::now() >= deadline {
            return panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// A `zellij` command pinned to a specific `XDG_RUNTIME_DIR`. Zellij locates
/// its server socket there, so the self-close test creates, inspects, and
/// tears down its session through this one runtime dir.
fn scoped_zellij(xdg: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new("zellij");
    cmd.env("XDG_RUNTIME_DIR", xdg);
    cmd
}

/// Tear down a runtime-scoped session even if an assertion panics first.
struct ScopedSessionCleanup {
    name: String,
    xdg: PathBuf,
}

impl Drop for ScopedSessionCleanup {
    fn drop(&mut self) {
        let _ = scoped_zellij(&self.xdg)
            .args(["delete-session", &self.name, "--force"])
            .output();
    }
}

/// Count a runtime-scoped session's non-plugin (terminal) panes. A session
/// whose tab has emptied answers `action list-panes` with only plugin panes
/// (tab/status bars); a torn-down session fails the call. Both map to zero.
fn session_nonplugin_count(xdg: &Path, name: &str) -> usize {
    scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|panes| {
            panes.as_array().map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        pane.get("is_plugin").and_then(|b| b.as_bool()) == Some(false)
                            && pane.get("is_suppressed").and_then(|b| b.as_bool()) != Some(true)
                    })
                    .count()
            })
        })
        .unwrap_or(0)
}

/// Count a runtime-scoped session's non-plugin panes in a named tab.
fn tab_nonplugin_count(xdg: &Path, name: &str, tab_name: &str) -> usize {
    scoped_zellij(xdg)
        .args(["--session", name, "action", "list-panes", "-j", "-a"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| serde_json::from_slice::<serde_json::Value>(&out.stdout).ok())
        .and_then(|panes| {
            panes.as_array().map(|panes| {
                panes
                    .iter()
                    .filter(|pane| {
                        pane.get("tab_name").and_then(|name| name.as_str()) == Some(tab_name)
                            && pane.get("is_plugin").and_then(|b| b.as_bool()) == Some(false)
                            && pane.get("is_suppressed").and_then(|b| b.as_bool()) != Some(true)
                    })
                    .count()
            })
        })
        .unwrap_or(0)
}

/// Poll until the session's non-plugin pane count equals `target`, or the
/// timeout elapses.
fn wait_for_nonplugin_panes(xdg: &Path, name: &str, target: usize, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if session_nonplugin_count(xdg, name) == target {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Poll until the named tab's non-plugin pane count equals `target`, or the
/// timeout elapses.
fn wait_for_nonplugin_panes_in_tab(
    xdg: &Path,
    name: &str,
    tab_name: &str,
    target: usize,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if tab_nonplugin_count(xdg, name, tab_name) == target {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn assert_sidebar_is_left_thirty_percent(session: &str) {
    let output = std::process::Command::new("zellij")
        .args(["--session", session, "action", "list-panes", "-j", "-a"])
        .output()
        .expect("list-panes geometry");
    assert!(output.status.success(), "list-panes geometry failed");
    let panes: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("list-panes geometry json");
    let panes = panes.as_array().expect("pane geometry array");
    let sidebar = panes
        .iter()
        .find(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("title").and_then(|value| value.as_str()) == Some("rimz-sidebar")
        })
        .expect("rimz-sidebar pane");
    let tab_id = sidebar
        .get("tab_id")
        .and_then(|value| value.as_u64())
        .expect("sidebar tab id");
    let columns = sidebar
        .get("pane_columns")
        .and_then(|value| value.as_u64())
        .expect("sidebar columns");
    let total_columns = panes
        .iter()
        .filter(|pane| {
            pane.get("is_plugin").and_then(|value| value.as_bool()) == Some(false)
                && pane.get("tab_id").and_then(|value| value.as_u64()) == Some(tab_id)
        })
        .filter_map(|pane| {
            Some(pane.get("pane_x")?.as_u64()? + pane.get("pane_columns")?.as_u64()?)
        })
        .max()
        .expect("tab width");
    assert_eq!(
        sidebar.get("pane_x").and_then(|value| value.as_u64()),
        Some(0),
        "sidebar should be the left pane",
    );
    assert!(
        columns * 100 <= total_columns * 35,
        "sidebar should occupy roughly 30% of the tab: {columns}/{total_columns}",
    );
}

/// `open_background_view` opens a dedicated, named tab for a managed command
/// and is idempotent on that tab name: a second call is a no-op.
#[test]
fn open_background_view_creates_named_tab_idempotently() {
    require_zellij!();

    let name = unique_session_name("bgview");
    let _session = ZellijSession::spawn(&name);

    let opts = rimz::mux::BackgroundViewOptions {
        session_name: name.clone(),
        cwd: std::env::temp_dir(),
        name: "rimz-rc".to_owned(),
        command: vec!["sleep".to_owned(), "120".to_owned()],
    };

    let first = ZellijBackend
        .open_background_view(&opts)
        .expect("first launch");
    assert_eq!(first, rimz::mux::BackgroundViewLaunch::Launched);
    assert!(
        wait_for_tab_named(&name, "rimz-rc"),
        "expected a rimz-rc tab after launch",
    );

    let second = ZellijBackend
        .open_background_view(&opts)
        .expect("second launch");
    assert_eq!(
        second,
        rimz::mux::BackgroundViewLaunch::AlreadyRunning,
        "relaunching into a session that already carries the view is a no-op",
    );
}

/// Poll `query-tab-names` until a tab named `tab_name` appears, or time out.
fn wait_for_tab_named(session: &str, tab_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let listed = std::process::Command::new("zellij")
            .args(["--session", session, "action", "query-tab-names"])
            .output();
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

/// `wake_sidebar` issues `zellij --session <name> pipe --name rimz::feed --
/// <payload>`. We assert the subprocess returns success even when no
/// pipe-aware client consumes the payload.
#[test]
fn wake_sidebar_pipe_invocation_succeeds() {
    require_zellij!();

    let name = unique_session_name("pipe");
    let _session = ZellijSession::spawn(&name);

    let payload = br#"{"kind":"ledger_delta","workspace_id":"ws_test","request_id":"req_test","protocol_version":"rimz.plugin.v2"}"#;
    ZellijBackend
        .wake_sidebar(&name, payload)
        .expect("wake_sidebar succeeds against a live zellij session");
}

/// `list_panes` parses `zellij action list-panes -j -a` JSON. A fresh
/// session has at least one terminal pane (the implicit shell).
#[test]
fn list_panes_with_session_returns_terminals() {
    require_zellij!();

    let name = unique_session_name("panes");
    let _session = ZellijSession::spawn(&name);

    // Poll until the fresh session exposes its implicit shell pane instead of
    // guessing a fixed settle delay.
    let panes = wait_for_pane_count(&name, 1);
    assert!(
        !panes.is_empty(),
        "expected ≥1 terminal pane in fresh session {name}, got {panes:?}",
    );
    for pane in &panes {
        assert_eq!(pane.pane_id.mux(), MuxName::Zellij);
        assert!(
            pane.pane_id.raw().starts_with("terminal_"),
            "list_panes should filter plugins out; got {}",
            pane.pane_id,
        );
        assert_eq!(pane.session_name, name);
        assert!(
            pane.command
                .as_deref()
                .is_some_and(|command| !command.is_empty()),
            "zellij should report pane_command into PaneRef::command: {pane:?}",
        );
        assert!(
            pane.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()),
            "zellij should report pane_cwd into PaneRef::cwd: {pane:?}",
        );
    }
}

/// Capability probe must parse the binary's version string and compare it
/// against `MIN_ZELLIJ_VERSION`. No session required.
#[test]
fn version_floor_parses_and_compares() {
    require_zellij!();

    let caps = zellij::capabilities().expect("capabilities() against a live zellij");
    let (maj, min, patch) = caps
        .parsed_version
        .expect("parsed_version is Some for any 0.41+ build");
    assert!(
        (maj, min, patch) >= zellij::MIN_ZELLIJ_VERSION,
        "test host has zellij {maj}.{min}.{patch}; M0b requires ≥ {:?}",
        zellij::MIN_ZELLIJ_VERSION,
    );
    assert!(caps.meets_min_version);
    assert!(caps.binary_version.contains("zellij"));
}
