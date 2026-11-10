use std::time::Duration;

use rimz::mux::{MuxBackend, PopupRequest};

use super::support::*;

/// The plain layout has no tab or status bar, so a 300x90 client views a
/// 300x90 tab: 60% and 50% would open 180x45, so the 160x40 ceiling binds on
/// both axes and the job's terminal is that size less the border. This also
/// pins that the Zellij CI runs reads an integer `--width` as cells. Zellij can
/// start the job before it sizes the pty, so the job samples until it is sized.
#[test]
fn palette_popup_opens_at_the_ceiling_on_a_large_client() {
    require_zellij!();
    let room = LiveZellijSession::new("popup-size");
    let cwd = tempfile::TempDir::new().expect("cwd");
    room.create_plain_background(cwd.path(), "600");
    let mut client = AttachedClient::attach(&room, 300, 90);
    // A CLI action follows the last client to type, so this one must type.
    client.send_line("popup size probe");
    let size_file = cwd.path().join("popup-size");
    room.backend()
        .open_popup(&PopupRequest {
            session_name: room.name().to_owned(),
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!(
                    "while :; do stty size >> '{}'; sleep 0.1; done",
                    size_file.display()
                ),
            ],
            cwd: cwd.path().to_path_buf(),
            ..Default::default()
        })
        .expect("open popup");
    let sized = |text: &String| text.lines().find(|line| *line != "0 0").map(str::to_owned);
    let samples = poll_until(
        Duration::from_secs(15),
        || std::fs::read_to_string(&size_file).map_err(|err| err.to_string()),
        |text| sized(text).is_some(),
        "popup job's sized terminal",
    );
    assert_eq!(
        sized(&samples).as_deref(),
        Some("38 158"),
        "stty prints rows then columns: {samples:?}"
    );
}
