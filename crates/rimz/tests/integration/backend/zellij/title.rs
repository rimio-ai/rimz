use std::path::Path;
use std::time::{Duration, Instant};

use rimz::mux::{MuxBackend, NamedKey, ZellijBackend};
use tempfile::TempDir;

use super::support::*;

#[test]
fn attached_terminal_title_ignores_shell_osc_title() {
    require_zellij!();

    let room = LiveZellijSession::new("native-title");
    let cwd = TempDir::new().expect("cwd tempdir");
    std::fs::write(room.path().join(".zshrc"), "").expect("disable zsh first-run menu");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let opts = sidebar_opts(room.name(), cwd.path(), stub, 120);
    publish_room_bin(room.path(), &opts);
    let backend = ZellijBackend::with_runtime_dir(room.path());
    backend.open_sidebar(&opts, None).expect("open_sidebar");

    let panes = wait_for_pane_count(room.path(), room.name(), 2);
    let work_pane = panes
        .iter()
        .find(|pane| pane.spawn_command.is_none())
        .expect("ordinary work shell")
        .pane_id
        .clone();
    let sidebar_pane = panes
        .iter()
        .find(|pane| pane.spawn_command.is_some())
        .expect("sidebar pane")
        .pane_id
        .clone();
    let mut client = AttachedClient::attach(&room, 120, 40);
    client.wait_until_focused(&work_pane, "work pane");
    client.press_alt_until('h', &sidebar_pane, "sidebar pane");
    client.press_alt_until('l', &work_pane, "work pane after sidebar");

    backend
        .send_keys(
            &work_pane,
            r#"printf '\033]2;marvin@evil:~/leak\007'; printf '\124\111\124\114\105\137\104\117\116\105\012'"#,
        )
        .expect("type title payload");
    backend
        .send_key(&work_pane, NamedKey::Enter)
        .expect("run title payload");
    let capture = poll_until(
        Duration::from_secs(10),
        || {
            backend
                .capture_pane(&work_pane, Some(20), false)
                .map(|capture| capture.raw_text)
                .map_err(|err| err.to_string())
        },
        |capture| capture.contains("TITLE_DONE"),
        "title payload sentinel",
    );

    let shell = rimz::harness::launch::user_shell_program();
    let shell = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sh");
    let expected = format!("{} | {shell}", room.name());
    let deadline = Instant::now() + Duration::from_secs(5);
    let (titles, output_len) = loop {
        let output = client.output_bytes();
        let titles = crate::common::osc_titles(&output);
        if titles.iter().any(|title| title == &expected) || Instant::now() >= deadline {
            break (titles, output.len());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert!(
        !titles
            .iter()
            .any(|title| title.contains("marvin@evil") || title.contains("~/leak")),
        "shell-controlled title reached the attached client: titles={titles:?}; capture={capture:?}",
    );
    assert!(
        titles.iter().any(|title| title == &expected),
        "attached client never received {expected:?}: titles={titles:?}; bytes={output_len}; capture={capture:?}",
    );
}
