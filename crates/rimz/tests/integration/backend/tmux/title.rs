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
        .rename_tab(
            session,
            &anchor,
            "#health ready",
            TabNameIntent::Rest {
                observed: server.display(anchor.raw(), "#{window_name}"),
            },
        )
        .expect("rename window");
    assert_eq!(server.window_names(session), ["#health ready"]);

    server
        .backend
        .open_tab(&TabOptions {
            env: Default::default(),
            title: "#host#health".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "30".to_owned()],
                    name: None,
                }])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar_opts(
                server._tempdir.path(),
                session,
                PathBuf::from("/bin/true"),
                Some(120),
            ),
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
fn rebuild_updates_the_record_only_while_the_observed_name_matches() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "rimz-rebuild-title";
    server.ensure_with_shell(session);
    let anchor = list_session_panes(&server, session)[0].pane_id.clone();
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "founder",
            TabNameIntent::Claim {
                pane_name: "founder".into(),
            },
        )
        .unwrap();
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "peer's:#h.2 ?",
            TabNameIntent::Rebuild {
                observed: "founder".to_owned(),
                base: "peer's-#h-2".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(
        server.display(anchor.raw(), "#{window_name}"),
        "peer's-#h-2 ?"
    );
    assert_eq!(
        server.display(anchor.raw(), "#{@rimz_tab_base}"),
        "peer's-#h-2"
    );
    assert_eq!(
        server.display(anchor.raw(), "#{@rimz_tab_founders}"),
        anchor.raw()
    );
    server.output(&["rename-window", "-t", anchor.raw(), "my tab"]);
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "stale",
            TabNameIntent::Rebuild {
                observed: "peer's-#h-2 ?".to_owned(),
                base: "stale".to_owned(),
            },
        )
        .unwrap();
    assert_eq!(server.display(anchor.raw(), "#{window_name}"), "my tab");
    assert_eq!(
        server.display(anchor.raw(), "#{@rimz_tab_base}"),
        "peer's-#h-2"
    );
}

#[test]
fn in_place_claim_pins_the_pane_name_not_the_scoped_tab_title() {
    require_tmux!();
    let session = "rimz-claim-title";
    let server = TmuxServer::new();
    let mut opts = session_opts(
        session,
        WorkspaceId::from_project_root(server._tempdir.path()),
        server._tempdir.path(),
        server._tempdir.path(),
        Some((120, 40)),
    );
    opts.config.theme = toml::from_str("meta = '#aabbcc'\naccent = '#778899'").unwrap();
    server.backend.ensure_session(&opts).unwrap();
    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(session, "#{pane_id}"));
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "shell ?",
            TabNameIntent::Status {
                observed: server.display(anchor.raw(), "#{window_name}"),
            },
        )
        .expect("status before claim");
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "#feat",
            TabNameIntent::Claim {
                pane_name: "opus.fast (@brave-fox)".into(),
            },
        )
        .expect("claim in-place pane");
    assert_eq!(server.display(anchor.raw(), "#{window_name}"), "#feat");
    assert_eq!(
        server.display(anchor.raw(), "#{@rimz_title}"),
        "opus.fast (@brave-fox)"
    );
    server.output(&["select-pane", "-t", anchor.raw(), "-T", "terminal,title"]);
    let roster = list_session_panes(&server, session);
    assert_eq!(
        roster
            .iter()
            .find(|pane| pane.pane_id == anchor)
            .expect("claimed pane")
            .title
            .as_deref(),
        Some("opus.fast (@brave-fox)"),
    );
    let format = server.show_option(&["-t", session], "set-titles-string");
    assert_eq!(
        server.display(anchor.raw(), &format),
        format!("{session} | opus.fast (@brave-fox)")
    );
    server.output(&[
        "set-option",
        "-p",
        "-t",
        anchor.raw(),
        "@rimz_kind",
        "claude",
    ]);
    let title_format = |format: &str| {
        String::from_utf8(
            server
                .output(&["display-message", "-p", "-t", anchor.raw(), format])
                .stdout,
        )
        .unwrap()
        .trim_end_matches('\n')
        .to_owned()
    };
    let kind = rimz::ids::AgentKind::new_unchecked("claude");
    for (launch, origin, lead, rest, tone) in [
        (
            rimz::agents::LaunchParams {
                team: Some("recon".into()),
                role: Some("planner".into()),
                ..Default::default()
            },
            "team",
            "recon",
            " · planner (@brave-fox)",
            "#aabbcc",
        ),
        (
            rimz::agents::LaunchParams {
                profile: Some("coder".into()),
                automation: Some(rimz::agents::Automation::Loop),
                ..Default::default()
            },
            "automation",
            "loop",
            " · coder (@brave-fox)",
            "#778899",
        ),
        (
            rimz::agents::LaunchParams {
                automation: Some(rimz::agents::Automation::Workflow {
                    workflow: "nightly".into(),
                    step: "build".into(),
                }),
                ..Default::default()
            },
            "automation",
            "nightly",
            " · build (@brave-fox)",
            "#778899",
        ),
    ] {
        let title = rimz::agents::pane_title::title(
            rimz::agents::pane_title::Seat::from_launch(&kind, Some("brave-fox"), &launch),
            &[],
        );
        let claim = |pane_name| {
            server
                .backend
                .rename_tab(
                    session,
                    &anchor,
                    "#feat",
                    TabNameIntent::Claim { pane_name },
                )
                .unwrap();
        };
        claim(title.clone());
        assert_eq!(
            server.display(anchor.raw(), "#{@rimz_title}"),
            format!("{lead}{rest}")
        );
        for (option, expected) in [
            ("#{@rimz_origin}", origin),
            ("#{@rimz_title_lead}", lead),
            ("#{@rimz_title_rest}", rest),
        ] {
            assert_eq!(title_format(option), expected, "{option}");
        }
        assert_eq!(
            title_format("#{E:pane-border-format}"),
            format!("#[fg={tone}] {lead}#[fg=#d97757]{rest} ")
        );
        claim("plain · literal (@brave-fox)".into());
        assert_eq!(
            server.display(anchor.raw(), "#{@rimz_title}"),
            "plain · literal (@brave-fox)"
        );
        for option in [
            "#{@rimz_origin}",
            "#{@rimz_title_lead}",
            "#{@rimz_title_rest}",
        ] {
            assert_eq!(server.display(anchor.raw(), option), "", "{option}");
        }
        assert_eq!(
            title_format("#{E:pane-border-format}"),
            "#[fg=#d97757] plain · literal (@brave-fox) "
        );
        claim(title);
    }
    let mut release_panes = vec![anchor.raw().to_owned()];
    for _ in 0..2 {
        let pane = server.stdout(&[
            "split-window",
            "-d",
            "-P",
            "-F",
            "#{pane_id}",
            "-t",
            anchor.raw(),
            "sleep 30",
        ]);
        for option in [
            "@rimz_title",
            "@rimz_origin",
            "@rimz_title_lead",
            "@rimz_title_rest",
        ] {
            let value = title_format(&format!("#{{{option}}}"));
            server.output(&["set-option", "-p", "-t", &pane, option, &value]);
        }
        release_panes.push(pane);
    }
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "sh",
            TabNameIntent::Release {
                observed: "#feat".to_owned(),
            },
        )
        .expect("release pin");
    for pane in release_panes {
        for option in [
            "#{@rimz_title}",
            "#{@rimz_origin}",
            "#{@rimz_title_lead}",
            "#{@rimz_title_rest}",
        ] {
            assert_eq!(server.display(&pane, option), "", "{pane} {option}");
        }
    }
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
            env: Default::default(),
            title: "#agents".to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec!["sleep".to_owned(), "30".to_owned()],
                    name: Some("codex (@swift-otter)".into()),
                }])],
                focused_pane: 0,
            },
            focus: true,
            dock_sidebar: true,
            after: None,
            sidebar: sidebar_opts(
                server._tempdir.path(),
                session,
                PathBuf::from("/bin/true"),
                Some(120),
            ),
        })
        .expect("open named pane tab");

    let target = format!("{session}:#agents");
    assert_eq!(
        server.display(
            &target,
            "#S | #{?#{@rimz_title},#{@rimz_title},#{pane_current_command}}",
        ),
        format!("{session} | codex (@swift-otter)"),
    );

    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(&target, "#{pane_id}"));
    server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Pane(anchor),
            cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
            command: Some(vec!["sleep".to_owned(), "30".to_owned()]),
            title: Some("coder (@swift-otter) › claude subagent (@gold-tower)".into()),
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
        format!("{session} | coder (@swift-otter) › claude subagent (@gold-tower)"),
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

    client.watch_output(b"TITLE_DONE");
    server
        .backend
        .send_keys(
            &work_pane, ANY_SESSION,
            r#"printf '\033]2;marvin@evil:~/leak\007'; printf '\124\111\124\114\105\137\104\117\116\105\012'"#,
        )
        .expect("type title payload");
    server
        .backend
        .send_key(&work_pane, ANY_SESSION, NamedKey::Enter)
        .expect("run title payload");
    let capture = capture_pane_until(
        &server.backend,
        &work_pane,
        "TITLE_DONE",
        Duration::from_secs(10),
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.output_seen() {
        assert!(
            Instant::now() < deadline,
            "attached client never rendered title payload sentinel: capture={capture:?}",
        );
        thread::sleep(Duration::from_millis(25));
    }
    let titles = client.titles();
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

/// A producer projects a rename from the name it observed and applies it
/// later; a launch claim that lands in between must survive it.
#[test]
fn projected_rename_keeps_a_claim_made_after_its_observation() {
    require_tmux!();
    let session = "rimz-stale-projection";
    let server = TmuxServer::new();
    ensure_rimz_session(&server, session, Some((120, 40)));
    let anchor = PaneId::from_parts(MuxName::Tmux, server.display(session, "#{pane_id}"));
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "shell ?",
            TabNameIntent::Status {
                observed: server.display(anchor.raw(), "#{window_name}"),
            },
        )
        .expect("status");
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "opus",
            TabNameIntent::Claim {
                pane_name: "opus".into(),
            },
        )
        .expect("claim");
    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "shell",
            TabNameIntent::Rest {
                observed: "shell ?".to_owned(),
            },
        )
        .expect("a stale projection is a silent miss");
    assert_eq!(server.display(anchor.raw(), "#{window_name}"), "opus");
    assert_eq!(server.display(anchor.raw(), "#{@rimz_title}"), "opus");

    server
        .backend
        .rename_tab(
            session,
            &anchor,
            "opus ?",
            TabNameIntent::Status {
                observed: "opus".to_owned(),
            },
        )
        .expect("current projection");
    assert_eq!(server.display(anchor.raw(), "#{window_name}"), "opus ?");
}
