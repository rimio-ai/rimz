use super::support::*;
use rimz::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN};

#[test]
fn large_tmux_paste_delivers_exact_bytes_and_deletes_buffers() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("large-paste");
    let pane = list_session_panes(&server, "large-paste")[0]
        .pane_id
        .clone();
    let ready = server._tempdir.path().join("reader-ready");
    let received = server._tempdir.path().join("received");
    let text = "- héllo\r\nworld\n\0\x1b[200~\r".repeat(2048);
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let expected = format!("{BRACKET_PASTE_OPEN}{normalized}{BRACKET_PASTE_CLOSE}").into_bytes();
    server.tmux(&[
        "respawn-pane",
        "-k",
        "-t",
        pane.raw(),
        "sh",
        "-c",
        &format!(
            "stty raw -echo; printf ready > '{}'; exec cat > '{}'",
            ready.display(),
            received.display()
        ),
    ]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "raw reader never became ready");
        thread::sleep(Duration::from_millis(10));
    }
    // Repeated calls exercise buffer lifetime and invocation-local naming.
    server.backend.paste_text(&pane, "").expect("empty paste");
    for in_copy_mode in [false, true] {
        if in_copy_mode {
            server.tmux(&["copy-mode", "-t", pane.raw()]);
            assert_eq!(server.display(pane.raw(), "#{pane_in_mode}"), "1");
        }
        server
            .backend
            .paste_text(&pane, &text)
            .expect("large paste");
    }
    assert_eq!(server.display(pane.raw(), "#{pane_in_mode}"), "1");
    let expected = [
        format!("{BRACKET_PASTE_OPEN}{BRACKET_PASTE_CLOSE}").into_bytes(),
        expected.repeat(2),
    ]
    .concat();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let actual = std::fs::read(&received).unwrap_or_default();
        if actual.len() >= expected.len() {
            assert_eq!(actual.len(), expected.len());
            assert_eq!(
                actual
                    .iter()
                    .zip(&expected)
                    .position(|(left, right)| left != right),
                None,
                "first mismatched paste byte"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "paste truncated at {} of {} bytes",
            actual.len(),
            expected.len()
        );
        thread::sleep(Duration::from_millis(10));
    }
    let output = server.output(&["list-buffers"]);
    assert!(output.stdout.is_empty(), "paste buffers must be deleted");
}

#[test]
fn failed_tmux_paste_deletes_buffer_for_dead_target() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("paste-survivor");
    server.ensure_with_shell("paste-target");
    let pane = list_session_panes(&server, "paste-target")[0]
        .pane_id
        .clone();
    server.tmux(&["kill-pane", "-t", pane.raw()]);
    server
        .backend
        .paste_text(&pane, "private message body")
        .expect_err("dead target rejects paste");
    let output = server.output(&["list-buffers"]);
    assert!(
        output.stdout.is_empty(),
        "failed paste buffer must be deleted"
    );
}
