use super::support::*;
use rimz::mux::tab_name::TabNameIntent;

#[test]
fn window_names_keep_literal_hashes() {
    require_tmux!();

    let session = "rimz-native-window-name";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(session, "#{pane_id}"));

    server
        .backend
        .rename_tab(session, &anchor, "#health ready", TabNameIntent::Rest)
        .expect("rename window");
    assert_eq!(server.window_names(session), ["#health ready"]);

    server
        .backend
        .open_tab(&TabOptions {
            title: "#host#health".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "30".to_owned()],
                    name: None,
                }])],
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar_opts(session, PathBuf::from("/bin/true"), Some(120)),
        })
        .expect("open named tab");
    assert!(
        server
            .window_names(session)
            .iter()
            .any(|name| name == "#host#health"),
        "opened window name did not preserve literal hashes: {:?}",
        server.window_names(session),
    );
    assert_eq!(
        server.display(&format!("{session}:#host#health"), "#{window_name}"),
        "#host#health",
        "literal hash names must remain valid tmux targets",
    );
}

#[test]
fn in_place_claim_pins_the_pane_name_not_the_scoped_tab_title() {
    require_tmux!();
    let session = "rimz-claim-title";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(session, "#{pane_id}"));
    server
        .backend
        .rename_tab(session, &anchor, "shell ?", TabNameIntent::Status)
        .expect("status before claim");
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "#feat",
            TabNameIntent::Claim {
                pane_name: "opus.fast".to_owned(),
            },
        )
        .expect("claim in-place pane");
    assert_eq!(server.display(anchor.raw(), "#{window_name}"), "#feat");
    assert_eq!(server.display(anchor.raw(), "#{@rimz_title}"), "opus-fast");
    server.output(&["select-pane", "-t", anchor.raw(), "-T", "terminal,title"]);
    let roster = list_session_panes(&server, session);
    assert_eq!(
        roster
            .iter()
            .find(|pane| pane.pane_id == anchor)
            .expect("claimed pane")
            .title
            .as_deref(),
        Some("opus-fast"),
    );
    assert_eq!(
        server.display(anchor.raw(), "#{@rimz_restore_automatic_rename}"),
        ""
    );
    let format = server.show_option(&["-t", session], "set-titles-string");
    assert_eq!(
        server.display(anchor.raw(), &format),
        format!("{session} | opus-fast")
    );
    server
        .backend
        .rename_tab(session, &anchor, "sh", TabNameIntent::Release)
        .expect("release pin");
    let roster = list_session_panes(&server, session);
    assert_eq!(
        roster
            .iter()
            .find(|pane| pane.pane_id == anchor)
            .expect("released pane")
            .title
            .as_deref(),
        Some("terminal_title"),
    );
}

#[test]
fn named_layout_pane_drives_the_terminal_title_format() {
    require_tmux!();

    let session = "rimz-native-named-title";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    server
        .backend
        .open_tab(&TabOptions {
            title: "#agents".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "30".to_owned()],
                    name: Some("codex".to_owned()),
                }])],
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar_opts(session, PathBuf::from("/bin/true"), Some(120)),
        })
        .expect("open named pane tab");

    let target = format!("{session}:#agents");
    assert_eq!(
        server.display(
            &target,
            "#S | #{?#{@rimz_title},#{@rimz_title},#{pane_current_command}}",
        ),
        format!("{session} | codex"),
    );

    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(&target, "#{pane_id}"));
    server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Pane(anchor),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
            title: Some("claude".to_owned()),
            close_on_exit: false,
            env: BTreeMap::new(),
            placement: SplitPlacement::Directional(rimz::mux::SplitDirection::Right),
            focus: true,
        })
        .expect("split named pane");
    assert_eq!(
        server.display(
            &target,
            "#S | #{?#{@rimz_title},#{@rimz_title},#{pane_current_command}}",
        ),
        format!("{session} | claude"),
    );
}

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
