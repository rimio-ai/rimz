use std::collections::BTreeMap;
use std::time::Duration;

use rimz::mux::{MuxBackend, SplitPaneOptions, SplitPlacement, SplitTarget, ZellijBackend};
use rimz::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN};

use super::support::*;

#[test]
fn large_zellij_paste_delivers_exact_pty_bytes() {
    require_zellij!();
    let room = LiveZellijSession::new("large-paste");
    let xdg = room.path();
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    wait_for_pane_count(xdg, room.name(), 1);
    let ready = xdg.join("reader-ready");
    let received = xdg.join("received");
    let text = "- héllo\r\nworld\n\0\x1b[200~\r".repeat(2048);
    let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
    let expected = format!("{BRACKET_PASTE_OPEN}{normalized}{BRACKET_PASTE_CLOSE}").into_bytes();
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Ambient,
            cwd: Some(xdg.to_string_lossy().into_owned()),
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!(
                    "stty raw -echo; printf ready > '{}'; exec cat > '{}'",
                    ready.display(),
                    received.display(),
                ),
            ]),
            title: Some("paste-reader".to_owned()),
            close_on_exit: false,
            env: BTreeMap::new(),
            placement: SplitPlacement::default(),
            focus: false,
        })
        .expect("open raw reader");
    let panes = wait_for_pane_count(xdg, room.name(), 2);
    let pane = panes
        .iter()
        .find(|pane| pane.title.as_deref() == Some("paste-reader"))
        .expect("reader pane")
        .pane_id
        .clone();
    poll_until(
        Duration::from_secs(10),
        || std::fs::read(&ready).map_err(|err| err.to_string()),
        |bytes| bytes == b"ready",
        "raw reader readiness",
    );
    backend.paste_text(&pane, &text).expect("large paste");
    let actual = poll_until(
        Duration::from_secs(10),
        || std::fs::read(&received).map_err(|err| err.to_string()),
        |bytes| bytes.len() >= expected.len(),
        "complete paste bytes",
    );
    assert_eq!(actual.len(), expected.len());
    assert_eq!(
        actual
            .iter()
            .zip(&expected)
            .position(|(left, right)| left != right),
        None,
        "first mismatched paste byte"
    );
}
