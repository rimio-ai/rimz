use super::support::*;
use rimz::ids::ViewId;
use rimz::mux::ViewSize;

#[test]
fn widening_fit_preserves_sidebar_width_and_delivers_one_final_pty_resize() {
    if let Some(path) = std::env::var_os("RIMZ_TEST_WINSIZE_FILE") {
        use std::io::Write;
        let mut signals =
            signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH]).unwrap();
        let tty = std::fs::File::open("/dev/tty").unwrap();
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(log, "ready").unwrap();
        for _ in signals.forever() {
            let size = rustix::termios::tcgetwinsize(&tty).unwrap();
            writeln!(log, "{} {}", size.ws_row, size.ws_col).unwrap();
        }
        return;
    }
    require_tmux!();
    let server = TmuxServer::new();
    let session = "hidden-sidebar-fit";
    server.ensure_with_shell(session);
    server.tmux(&["set-option", "-t", session, "status", "off"]);
    let _client = AttachedTmuxClient::attach(&server.socket, session, 180, 50);
    for shape in ["zoomed", "alone", "duplicate", "normal"] {
        let log = server._tempdir.path().join(format!("{shape}-resizes"));
        let env = format!("RIMZ_TEST_WINSIZE_FILE={}", log.display());
        let sidebar = server.stdout(&[
            "new-window", "-d", "-P", "-F", "#{pane_id}", "-t", session, "-e", &env,
            std::env::current_exe().unwrap().to_str().unwrap(), "--exact",
            "backend::tmux::hidden_geometry::widening_fit_preserves_sidebar_width_and_delivers_one_final_pty_resize", "--nocapture",
        ]);
        let view = ViewId::new_unchecked(server.display(&sidebar, "#{window_id}"));
        server.tmux(&["select-pane", "-t", &sidebar, "-T", "rimz-sidebar"]);
        if shape != "alone" {
            let sibling = server.stdout(&[
                "split-window",
                "-d",
                "-h",
                "-t",
                &sidebar,
                "-P",
                "-F",
                "#{pane_id}",
            ]);
            server.tmux(&["resize-pane", "-t", &sidebar, "-x", "40"]);
            if shape == "duplicate" {
                server.tmux(&["select-pane", "-t", &sibling, "-T", "rimz-sidebar"]);
            }
        }
        if shape == "zoomed" {
            server.tmux(&["resize-pane", "-Z", "-t", &sidebar]);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("ready")
        {
            assert!(Instant::now() < deadline, "resize observer did not start");
            thread::sleep(Duration::from_millis(10));
        }
        // Drain the setup's resize throttle before measuring the fit's queue.
        thread::sleep(Duration::from_millis(350));
        std::fs::write(&log, "").unwrap();
        let rows = server
            .backend
            .fit_view(
                &view,
                ViewSize {
                    cols: 240,
                    rows: 65,
                },
                std::num::NonZeroU16::new(60).unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            server.display(view.as_str(), "#{window_width} #{window_height}"),
            "240 65"
        );
        if shape == "normal" {
            assert_eq!(
                server.display(&sidebar, "#{pane_width}"),
                "60",
                "a widening fit must land the sidebar at its legit width"
            );
            thread::sleep(Duration::from_millis(350));
            assert_eq!(
                std::fs::read_to_string(&log).unwrap(),
                "65 60\n",
                "the sidebar must observe only the final resize, not a transient wide frame"
            );
            assert_eq!(
                rows.get(&PaneId::from_parts(MuxName::Tmux, &sidebar)),
                Some(&rimz::mux::FittedPane {
                    rows: 65,
                    cols: Some(60)
                })
            );
        } else {
            assert!(
                rows.values().all(|pane| pane.cols.is_none()),
                "a skipped width reassertion supplies rows only"
            );
            assert_ne!(
                server.display(&sidebar, "#{pane_width}"),
                "60",
                "ambiguous, solitary, and zoomed sidebars keep native width handling"
            );
            assert_eq!(
                server.display(view.as_str(), "#{window_zoomed_flag}"),
                if shape == "zoomed" { "1" } else { "0" }
            );
        }
    }
}

#[test]
fn ineligible_fit_reports_skipped_without_live_rows() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "hidden-fit-skips";
    server.ensure_with_shell(session);
    let client = AttachedTmuxClient::attach(&server.socket, session, 180, 50);
    let current = ViewId::new_unchecked(server.display(session, "#{window_id}"));
    let target = ViewSize {
        cols: 240,
        rows: 65,
    };
    let cols = std::num::NonZeroU16::new(60).unwrap();
    assert_eq!(
        server.backend.fit_view(&current, target, cols).unwrap(),
        None,
        "a selected view is skipped, not fitted"
    );
    let hidden = ViewId::new_unchecked(server.stdout(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        session,
    ]));
    server.tmux(&[
        "set-window-option",
        "-t",
        hidden.as_str(),
        "window-size",
        "manual",
    ]);
    assert_eq!(
        server.backend.fit_view(&hidden, target, cols).unwrap(),
        None
    );
    server.tmux(&[
        "set-window-option",
        "-u",
        "-t",
        hidden.as_str(),
        "window-size",
    ]);
    drop(client);
    assert_eq!(
        server.backend.fit_view(&hidden, target, cols).unwrap(),
        None
    );
}

#[test]
fn hidden_views_fit_to_current_size_preserving_options_and_pty_rows() {
    require_tmux!();
    let server = TmuxServer::new();
    let session = "hidden-fit";
    server.ensure_with_shell(session);
    server.tmux(&["set-option", "-t", session, "status", "off"]);
    let client = AttachedTmuxClient::attach(&server.socket, session, 180, 50);
    let current = server.display(session, "#{window_id}");
    let mut hidden = Vec::new();
    for _ in 0..3 {
        hidden.push(server.stdout(&[
            "new-window",
            "-d",
            "-P",
            "-F",
            "#{window_id}",
            "-t",
            session,
        ]));
    }
    // Put two hidden windows on the MRU stack; leave the first never visited.
    for view in [&hidden[1], &hidden[2], &current] {
        server.tmux(&["select-window", "-t", view]);
    }
    server.tmux(&[
        "set-window-option",
        "-t",
        &hidden[1],
        "window-size",
        "largest",
    ]);
    server.tmux(&["resize-window", "-t", &hidden[2], "-x", "170", "-y", "45"]);
    client
        ._master
        .resize(PtySize {
            cols: 220,
            rows: 65,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.display(&current, "#{window_width} #{window_height}") != "220 65" {
        assert!(Instant::now() < deadline, "current window did not grow");
        thread::sleep(Duration::from_millis(10));
    }
    let lag = server.backend.lagging_views(session).unwrap();
    assert!(
        lag.is_some(),
        "hidden windows must be discovered before selection"
    );
    let lag = lag.unwrap();
    assert_eq!(
        lag.target,
        ViewSize {
            cols: 220,
            rows: 65
        }
    );
    assert_eq!(
        lag.views,
        [&hidden[1], &hidden[0]].map(ViewId::new_unchecked)
    );
    for (view, local) in [(&hidden[1], "largest"), (&hidden[0], "")] {
        let pane = PaneId::from_parts(MuxName::Tmux, server.display(view, "#{pane_id}"));
        assert_eq!(
            server
                .backend
                .lagging_view(session, &pane)
                .unwrap()
                .unwrap()
                .view
                .as_str(),
            view
        );
        let rows = server
            .backend
            .fit_view(
                &ViewId::new_unchecked(view),
                lag.target,
                std::num::NonZeroU16::new(60).unwrap(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            server.display(view, "#{window_width} #{window_height}"),
            "220 65"
        );
        assert_eq!(
            server.stdout(&["show-options", "-wqv", "-t", view, "window-size"]),
            local
        );
        let expected: u16 = server
            .display(pane.raw(), "#{pane_height}")
            .parse()
            .unwrap();
        assert_eq!(
            rows.get(&pane),
            Some(&rimz::mux::FittedPane {
                rows: expected,
                cols: None
            })
        );
        let tty = std::fs::File::open(server.display(pane.raw(), "#{pane_tty}")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while rustix::termios::tcgetwinsize(&tty).unwrap().ws_row != expected {
            assert!(
                Instant::now() < deadline,
                "PTY rows must match the fitted pane"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(server.backend.lagging_view(session, &pane).unwrap(), None);
    }
    assert!(
        server
            .backend
            .lagging_views(session)
            .unwrap()
            .unwrap()
            .views
            .is_empty()
    );
    drop(client);
    let _watch = rimz::mux::tmux::PresenceWatch::attach(&server.socket, session).unwrap();
    server.wait_for_control_client(session);
    assert_eq!(
        server.backend.lagging_views(session).unwrap(),
        None,
        "an ignore-size host watch is not a sizing client"
    );
}
