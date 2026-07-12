#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

#[test]
fn split_pane_injects_env_vars() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("split");
    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    server
        .backend
        .split_pane(SplitPaneOptions {
            session_name: None,
            target_view_id: None,
            target_pane_id: None,
            cwd: None,
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf RIMZ_TEST_VAR=$RIMZ_TEST_VAR; sleep 5".to_owned(),
            ]),
            env,
            stacked: false,
            direction: Default::default(),
            focus: false,
        })
        .expect("split_pane");
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("split".to_owned()),
            ..Default::default()
        })
        .expect("list_panes after split")
        .panes;
    assert_eq!(
        panes.len(),
        2,
        "expected 2 panes after split, got {panes:?}"
    );
    let new_pane = panes
        .iter()
        .find(|p| p.pane_id.raw() != "%0")
        .expect("split created a new pane id");
    assert!(
        new_pane
            .spawn_command
            .as_deref()
            .is_some_and(|command| command.contains("printf RIMZ_TEST_VAR")),
        "split pane should expose its birth command, got {new_pane:?}",
    );
    let capture = capture_pane_until(
        &server.backend,
        &new_pane.pane_id,
        "marker-rimz-env",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("marker-rimz-env"),
        "split-pane should expose RIMZ_TEST_VAR; capture was: {capture:?}",
    );
}

#[test]
fn floating_panes_are_classified_and_closed_with_their_view() {
    require_tmux!();
    let server = TmuxServer::new();
    if !server.supports_floating_panes() {
        eprintln!("tmux predates 3.7 floating panes; skipping test");
        return;
    }

    server.ensure_with_shell("floating");
    let anchor = list_session_panes(&server, "floating")[0].pane_id.clone();
    server.tmux(&["new-pane", "-d", "-t", anchor.raw(), "sleep", "120"]);

    let floating = list_session_panes(&server, "floating")
        .into_iter()
        .find(|pane| pane.is_floating)
        .expect("tmux 3.7 floating pane is classified");
    assert_ne!(floating.pane_id, anchor);

    assert_eq!(
        server
            .backend
            .close_view_floating_panes("floating", &anchor)
            .expect("close floating panes"),
        vec![floating.pane_id],
    );
    assert!(
        list_session_panes(&server, "floating")
            .iter()
            .all(|pane| !pane.is_floating)
    );
}

/// `send_keys`, `send_key`, `paste_text`, and `capture_pane` round-trip through
/// a live pane.

#[test]
fn pane_io_round_trips_keys_named_keys_and_bracketed_paste() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("io");
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("io".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    let pane_id = panes[0].pane_id.clone();
    server
        .backend
        .send_keys(&pane_id, "printf rimz-marker-io\n")
        .expect("send_keys");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-io",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-io"),
        "expected marker in capture, got: {capture:?}",
    );
    server
        .backend
        .send_keys(&pane_id, "printf rimz-marker-key")
        .expect("send_keys");
    server
        .backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("send_key");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-key",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-key"),
        "expected marker in capture, got: {capture:?}",
    );
    let key_bytes = server._tempdir.path().join("named-key-bytes");
    server
        .backend
        .send_keys(
            &pane_id,
            &format!(
                "stty raw -echo; dd bs=1 count=4 of={} 2>/dev/null; stty sane",
                key_bytes.display()
            ),
        )
        .expect("type raw key reader");
    server
        .backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("start raw key reader");
    thread::sleep(Duration::from_millis(100));
    server
        .backend
        .send_key(&pane_id, NamedKey::Escape)
        .expect("send escape");
    server
        .backend
        .send_key(&pane_id, NamedKey::ShiftTab)
        .expect("send shift-tab");
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = loop {
        if let Ok(bytes) = std::fs::read(&key_bytes)
            && bytes.len() == 4
        {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "named keys did not reach tmux pane"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(bytes, b"\x1b\x1b[Z");
    // Leading dash guards the `send-keys -l --` spelling: payload bytes must not
    // be re-read as tmux flags or key names.
    let payload = "-rf rimz-paste-marker";
    server
        .backend
        .paste_text(&pane_id, payload)
        .expect("paste_text");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-paste-marker",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains(payload),
        "the pasted payload should arrive contiguous and byte-safe, got: {capture:?}",
    );
}

/// Presence watch stays writable as the sole client so send-keys still works.

#[test]
fn send_keys_works_with_presence_watch_as_only_client() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("headless");
    let pane_id = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("headless".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes[0]
        .pane_id
        .clone();
    let _watch = rimz::mux::tmux::PresenceWatch::attach(Some(&server.socket), "headless")
        .expect("attach control client");
    server.wait_for_control_client("headless");
    server
        .backend
        .send_keys(&pane_id, "printf rimz-watch-send\n")
        .expect("send_keys under presence watch");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-watch-send",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-watch-send"),
        "send_keys should work when the presence watch is the only client, got: {capture:?}",
    );
}
