use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use rimz::mux::{ClientFocusOptions, MuxBackend, NamedKey, SplitPaneOptions, ZellijBackend};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

use super::support::*;

#[test]
fn sidebar_focus_command_targets_session_from_outside_room() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focuscmd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    let opts = sidebar_opts(&name, cwd.path(), stub, 200);
    backend.open_sidebar(&opts, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let sidebar = raw_sidebar_pane(xdg.path(), &name);
    let sidebar_id = sidebar.id;
    let tab_id = sidebar.tab_id;
    let work_id = expect_list_panes(xdg.path(), &name)
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == tab_id && !pane.is_sidebar())
        .map(|pane| pane.id)
        .expect("work pane id");

    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);
    focus_nonplugin_pane_until(xdg.path(), &name, tab_id, work_id, "fixture work pane");

    let env = Env::new();
    let workspace_root = std::path::PathBuf::from(format!("/tmp/rimz-{name}"));
    record_known_workspace_session(
        &env.state_root(),
        &opts.workspace_id,
        &workspace_root,
        &name,
    );
    write_topology_cache_from_list_panes(xdg.path(), &opts.workspace_id, &name);
    let run_focus_toggle = || {
        let output = env
            .rimz()
            .env("XDG_RUNTIME_DIR", xdg.path())
            .env("XDG_CACHE_HOME", xdg.path())
            .env("TMPDIR", xdg.path())
            .args([
                "--mux",
                "zellij",
                "sidebar",
                "focus",
                "--toggle",
                "--session-name",
                &name,
            ])
            .bounded_output()
            .expect("rimz sidebar focus");
        assert!(
            output.status.success(),
            "rimz sidebar focus failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    };

    run_focus_toggle();
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(xdg.path(), &name, tab_id, sidebar_id),
        Some(sidebar_id),
        "out-of-session focus should land on the sidebar pane",
    );

    write_topology_cache_from_list_panes(xdg.path(), &opts.workspace_id, &name);
    run_focus_toggle();
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(xdg.path(), &name, tab_id, work_id),
        Some(work_id),
        "toggle should return focus to the work pane in the sidebar tab",
    );
}
#[test]
fn split_pane_injects_env_vars() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("splitenv");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let marker_file = cwd.path().join("rimz-env-marker");

    // Birth a live background session with one long-lived pane to split from.
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let target = wait_for_pane_count(xdg.path(), &name, 1)[0].pane_id.clone();
    assert!(
        !target.raw().is_empty(),
        "session should have its working pane before the split",
    );

    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    ZellijBackend::with_runtime_dir(xdg.path())
        .split_pane(SplitPaneOptions {
            target_pane_id: Some(target),
            cwd: None,
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!(
                    "printf '%s' \"$RIMZ_TEST_VAR\" > {}; sleep 5",
                    marker_file.display()
                ),
            ]),
            env,
            direction: Default::default(),
            focus: false,
        })
        .expect("split_pane");

    let deadline = Instant::now() + Duration::from_secs(10);
    let marker = loop {
        if let Ok(text) = std::fs::read_to_string(&marker_file)
            && !text.is_empty()
        {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "env-injected split never wrote the marker file",
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        marker, "marker-rimz-env",
        "Zellij split pane missed the injected RIMZ_TEST_VAR",
    );
}
/// `paste_text` writes one bracketed paste (`ESC[200~` … `ESC[201~`) wrapping
/// the payload as a raw decimal byte list — the message delivery path. A
/// bare shell renders the markers literally, so the inner text still lands in
/// the pane; assert it arrives byte-for-byte. A leading dash is the regression
/// guard: the byte-write path must never re-read the payload as a flag or key.
#[test]
fn paste_text_delivers_the_literal_payload() {
    require_zellij!();

    let session = ZellijSession::spawn(unique_session_name("paste"));
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let panes = wait_for_pane_count(session.xdg.path(), &session.name, 1);
    let pane_id = panes[0].pane_id.clone();

    let payload = "-rf rimz-paste-marker";
    backend.paste_text(&pane_id, payload).expect("paste_text");

    let deadline = Instant::now() + Duration::from_secs(5);
    let captured = loop {
        let text = backend
            .capture_pane(&pane_id, None, false)
            .map(|capture| capture.raw_text)
            .unwrap_or_default();
        if text.contains(payload) || Instant::now() >= deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        captured.contains(payload),
        "the pasted payload should arrive contiguous and byte-safe, got: {captured:?}",
    );
}

#[test]
fn semantic_answer_keys_reach_a_live_pane() {
    require_zellij!();

    let session = ZellijSession::spawn(unique_session_name("answerkeys"));
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let panes = wait_for_pane_count(session.xdg.path(), &session.name, 1);
    let pane_id = panes[0].pane_id.clone();
    let marker_dir = TempDir::new().expect("marker dir");
    let key_bytes = marker_dir.path().join("named-key-bytes");

    backend
        .send_keys(
            &pane_id,
            &format!(
                "stty raw -echo; dd bs=1 count=4 of={} 2>/dev/null; stty sane",
                key_bytes.display()
            ),
        )
        .expect("type raw key reader");
    backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("start raw key reader");
    std::thread::sleep(Duration::from_millis(100));
    backend
        .send_key(&pane_id, NamedKey::Escape)
        .expect("send escape");
    backend
        .send_key(&pane_id, NamedKey::ShiftTab)
        .expect("send shift-tab");

    let deadline = Instant::now() + Duration::from_secs(5);
    let bytes = loop {
        if let Ok(bytes) = std::fs::read(&key_bytes)
            && bytes.len() == 4
        {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "named keys did not reach Zellij pane"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(bytes, b"\x1b\x1b[Z");
}
/// `client_view` reads each client's focused pane from `list-clients`.
/// A background session with no client focuses nothing; an attached client
/// focuses its terminal pane. Drives the hook-ingestion pane-recovery probe.
#[test]
fn client_view_tracks_the_attached_client() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    // Birth a background session: it exists and answers actions, but has no
    // attached client yet.
    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name])
        .bounded_output()
        .expect("attach --create-background");
    assert!(
        created.status.success(),
        "create-background failed: {}",
        String::from_utf8_lossy(&created.stderr),
    );
    wait_until_session_ready(xdg.path(), &name);

    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    // `--create-background` births the session without attaching, but the
    // bootstrap client that created it can still surface in `list-clients` for a
    // beat before it detaches — a window that widens under load. Poll until the
    // roster drains, then assert the steady state: a background session with no
    // client focuses nothing. A real regression (a detached session that keeps a
    // focused client) never drains and still fails here.
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let detached = loop {
        let panes = backend
            .client_view(ClientFocusOptions {
                session_name: Some(name.clone()),
                ..Default::default()
            })
            .map(|view| view.viewed_panes)
            .expect("client_view detached");
        if panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(
        detached.is_empty(),
        "a background session with no client focuses nothing: {detached:?}",
    );

    // Attach a client; its focused terminal pane is now reported.
    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    wait_for_attached_client(xdg.path(), &name);
    let pane_id = wait_for_pane_count(xdg.path(), &name, 1)[0].pane_id.clone();

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let focused = loop {
        let panes = backend
            .client_view(ClientFocusOptions {
                session_name: Some(name.clone()),
                ..Default::default()
            })
            .map(|view| view.viewed_panes)
            .expect("client_view attached");
        if !panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        focused.len(),
        1,
        "one attached client focuses one pane: {focused:?}",
    );
    assert_eq!(
        focused[0], pane_id,
        "the attached client focuses the session's lone terminal pane: {focused:?}",
    );
}
