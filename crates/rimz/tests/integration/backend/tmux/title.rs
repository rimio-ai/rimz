use super::support::*;

#[test]
fn attached_terminal_title_ignores_shell_osc_title() {
    require_tmux!();

    let session = "rimz-native-title";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    let work_pane = list_session_panes(&server, session)
        .into_iter()
        .next()
        .expect("work shell")
        .pane_id;
    let client = AttachedTmuxClient::attach(&server.socket, session, 120, 40);

    server
        .backend
        .send_keys(
            &work_pane,
            r#"printf '\033]2;marvin@evil:~/leak\007'; printf '\124\111\124\114\105\137\104\117\116\105\012'"#,
        )
        .expect("type title payload");
    server
        .backend
        .send_key(&work_pane, NamedKey::Enter)
        .expect("run title payload");
    let capture = capture_pane_until(
        &server.backend,
        &work_pane,
        "TITLE_DONE",
        Duration::from_secs(10),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    let titles = loop {
        let titles = crate::common::osc_titles(&client.output_bytes());
        if !titles.is_empty() || Instant::now() >= deadline {
            break titles;
        }
        thread::sleep(Duration::from_millis(25));
    };
    let prefix = format!("{session} | ");
    assert!(
        titles.iter().any(|title| title.starts_with(&prefix)),
        "attached client received no RimZ session title: titles={titles:?}; capture={capture:?}",
    );
    assert!(
        !titles
            .iter()
            .any(|title| title.contains("marvin@evil") || title.contains("~/leak")),
        "shell-controlled title reached the attached client: titles={titles:?}; capture={capture:?}",
    );
}
