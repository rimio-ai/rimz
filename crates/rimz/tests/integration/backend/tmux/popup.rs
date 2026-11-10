use rimz::mux::PopupRequest;

use super::support::*;

/// A 300x90 client gives a 300x89 window (the status row): 60% and 50% would
/// open 180x44, so the 160x40 ceiling binds on both axes and the job's
/// terminal is that size less the border.
#[test]
fn palette_popup_opens_at_the_ceiling_on_a_large_client() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "popup-size";
    server.ensure_with_shell(session);
    let _client = AttachedTmuxClient::attach(&server.socket, session, 300, 90);
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        let row = server.stdout(&[
            "list-clients",
            "-F",
            "#{client_name} #{client_width} #{window_width}",
        ]);
        if let Some(name) = row.strip_suffix(" 300 300") {
            break name.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "client never reached 300 columns: {row:?}"
        );
        thread::sleep(Duration::from_millis(25));
    };
    let size_file = server._tempdir.path().join("popup-size");
    server
        .backend
        .open_popup(&PopupRequest {
            session_name: session.to_owned(),
            program: "sh".to_owned(),
            args: vec![
                "-c".to_owned(),
                format!("stty size > '{}'", size_file.display()),
            ],
            cwd: std::env::temp_dir(),
            client: Some(client),
            ..Default::default()
        })
        .expect("open popup");
    let deadline = Instant::now() + Duration::from_secs(10);
    let size = loop {
        let text = std::fs::read_to_string(&size_file).unwrap_or_default();
        if text.ends_with('\n') {
            break text;
        }
        assert!(Instant::now() < deadline, "popup job never wrote its size");
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(size.trim(), "38 158", "stty prints rows then columns");
}
