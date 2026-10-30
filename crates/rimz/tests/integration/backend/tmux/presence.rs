#![allow(clippy::print_stdout)]

use super::support::*;

fn recv_presence_line_until<F>(
    rx: &std::sync::mpsc::Receiver<Option<rimz::mux::tmux::ControlLine>>,
    budget: Duration,
    label: &str,
    mut matches: F,
) -> rimz::mux::tmux::ControlLine
where
    F: FnMut(&rimz::mux::tmux::ControlLine) -> bool,
{
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(Some(line)) if matches(&line) => return line,
            Ok(Some(_)) => {}
            Ok(None) => panic!("presence stream ended before {label}"),
            Err(err) => panic!("timed out waiting for {label}: {err}"),
        }
    }
}

fn spawn_presence_drain(
    mut watch: rimz::mux::tmux::PresenceWatch,
) -> (
    std::sync::mpsc::Receiver<Option<rimz::mux::tmux::ControlLine>>,
    std::thread::JoinHandle<()>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<Option<rimz::mux::tmux::ControlLine>>();
    let drain = thread::spawn(move || {
        while let Some(line) = watch.next_line() {
            let _ = tx.send(Some(line));
        }
        let _ = tx.send(None);
    });
    (rx, drain)
}

fn wait_for_presence_stream_end(
    rx: &std::sync::mpsc::Receiver<Option<rimz::mux::tmux::ControlLine>>,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Some(_)) => {}
            Ok(None) => return,
            Err(err) => panic!("a dead server did not end the stream: {err}"),
        }
    }
}

#[test]
fn presence_watch_filters_client_attaches_and_switches_by_session() {
    use rimz::mux::tmux::{ControlLine, PresenceWatch};

    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("watched");
    server.tmux(&["new-session", "-d", "-s", "other", "sleep 120"]);
    let watch = PresenceWatch::attach(&server.socket, "watched").unwrap();
    let (rx, drain) = spawn_presence_drain(watch);
    recv_presence_line_until(&rx, Duration::from_secs(5), "attach seed", |line| {
        matches!(line, ControlLine::SeedPane { .. })
    });
    let other = AttachedTmuxClient::attach(&server.socket, "other", 80, 25);
    let deadline = Instant::now() + Duration::from_secs(5);
    while server.display("other", "#{session_attached}") != "1" {
        assert!(Instant::now() < deadline, "other client did not attach");
        thread::sleep(Duration::from_millis(10));
    }
    // A subscription update fences notifications from the preceding attach.
    server.tmux(&["respawn-pane", "-k", "-t", "watched:0", "sleep 120"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "other attach fence", |line| {
        assert!(
            !matches!(line, ControlLine::ClientSessionChanged { .. }),
            "{line:?}"
        );
        matches!(line, ControlLine::Subscription { command: Some(command), .. } if command == "sleep")
    });
    let watched = AttachedTmuxClient::attach(&server.socket, "watched", 80, 25);
    recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "watched attach",
        |line| matches!(line, ControlLine::ClientSessionChanged { session } if session == "watched"),
    );
    let attached_pid = watched.child.process_id().unwrap().to_string();
    let clients = server.stdout(&["list-clients", "-t", "watched", "-F", "#{client_pid}"]);
    assert!(
        clients.lines().any(|pid| pid == attached_pid),
        "the attaching client must be listed when its attach event arrives: {clients}"
    );
    let other_client = server.stdout(&["list-clients", "-t", "other", "-F", "#{client_name}"]);
    server.tmux(&["switch-client", "-c", &other_client, "-t", "watched"]);
    recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "watched switch",
        |line| matches!(line, ControlLine::ClientSessionChanged { session } if session == "watched"),
    );
    let switched_pid = other.child.process_id().unwrap().to_string();
    let clients = server.stdout(&["list-clients", "-t", "watched", "-F", "#{client_pid}"]);
    assert!(
        clients.lines().any(|pid| pid == switched_pid),
        "the switched client must already be listed: {clients}"
    );
    server.tmux(&["switch-client", "-c", &other_client, "-t", "other"]);
    server.tmux(&["respawn-pane", "-k", "-t", "watched:0", "sh"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "other switch fence", |line| {
        assert!(
            !matches!(line, ControlLine::ClientSessionChanged { .. }),
            "{line:?}"
        );
        matches!(line, ControlLine::Subscription { command: Some(command), .. } if command == "sh")
    });
    server.tmux(&["kill-server"]);
    wait_for_presence_stream_end(&rx);
    drain.join().unwrap();
}

#[test]
fn presence_watch_seeds_current_window_before_hidden_pane_activation() {
    use rimz::mux::tmux::{ControlLine, PresenceWatch};

    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("seed-focus");
    let current_window = server.display("seed-focus:0", "#{window_id}");
    let session_id = server.display("seed-focus:0", "#{session_id}");
    server.tmux(&["new-window", "-d", "-t", "seed-focus", "-n", "hidden", "sh"]);
    server.tmux(&["split-window", "-d", "-t", "seed-focus:hidden", "sh"]);
    let hidden_window = server.display("seed-focus:hidden", "#{window_id}");
    let hidden_pane = server.display("seed-focus:hidden.1", "#{pane_id}");
    let watch = PresenceWatch::attach(&server.socket, "seed-focus").expect("attach control client");
    let (rx, drain) = spawn_presence_drain(watch);
    let mut seeds = Vec::new();
    for _ in 0..3 {
        seeds.push(recv_presence_line_until(
            &rx,
            Duration::from_secs(5),
            "attach seed",
            |line| {
                assert!(
                    !matches!(line, ControlLine::SessionWindowChanged { .. }),
                    "attach must not need a window switch: {line:?}"
                );
                matches!(line, ControlLine::SeedPane { .. })
            },
        ));
    }
    assert!(
        seeds.iter().any(|line| matches!(line,
            ControlLine::SeedPane { session, window, window_active: true, .. }
                if session == &session_id && window == &current_window
        )),
        "attach must seed the current window: {seeds:?}"
    );
    assert_eq!(
        seeds
            .iter()
            .filter(|line| matches!(line,
                ControlLine::SeedPane { session, window, window_active: false, .. }
                    if session == &session_id && window == &hidden_window
            ))
            .count(),
        2,
        "hidden-window seeds: {seeds:?}"
    );
    server.tmux(&["select-pane", "-t", &hidden_pane]);
    recv_presence_line_until(
        &rx,
        Duration::from_secs(1),
        "hidden pane activation",
        |line| {
            assert!(
                !matches!(line, ControlLine::SessionWindowChanged { .. }),
                "hidden activation must not switch windows: {line:?}"
            );
            matches!(line, ControlLine::WindowPaneChanged { window, pane }
            if window == &hidden_window && pane == &hidden_pane)
        },
    );
    assert_eq!(server.display("seed-focus", "#{window_id}"), current_window);
    server.tmux(&["kill-server"]);
    wait_for_presence_stream_end(&rx);
    drain.join().expect("drain thread");
}

#[test]
fn focus_pane_reports_hidden_pane_activation_before_window_switch() {
    use rimz::mux::tmux::{ControlLine, PresenceWatch};

    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("focus-order");
    server.tmux(&[
        "new-window",
        "-d",
        "-t",
        "focus-order",
        "-n",
        "hidden",
        "sh",
    ]);
    server.tmux(&["split-window", "-d", "-t", "focus-order:hidden", "sh"]);
    let hidden_window = server.display("focus-order:hidden", "#{window_id}");
    let hidden_pane = server.display("focus-order:hidden.1", "#{pane_id}");
    let session_id = server.display("focus-order", "#{session_id}");
    let watch =
        PresenceWatch::attach(&server.socket, "focus-order").expect("attach control client");
    let (rx, drain) = spawn_presence_drain(watch);
    for _ in 0..3 {
        recv_presence_line_until(&rx, Duration::from_secs(5), "attach seed", |line| {
            matches!(line, ControlLine::SeedPane { .. })
        });
    }

    server
        .backend
        .focus_pane(&PaneId::from_parts(MuxName::Tmux, &hidden_pane), None)
        .expect("focus_pane");
    let first = recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "pane activation or window switch",
        |line| {
            matches!(line, ControlLine::WindowPaneChanged { window, pane }
                if window == &hidden_window && pane == &hidden_pane)
                || matches!(line, ControlLine::SessionWindowChanged { session, window }
                    if session == &session_id && window == &hidden_window)
        },
    );
    assert!(
        matches!(first, ControlLine::WindowPaneChanged { .. }),
        "the target pane must become active before its window is shown: {first:?}",
    );
    recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "window switch after pane activation",
        |line| {
            matches!(line, ControlLine::SessionWindowChanged { session, window }
            if session == &session_id && window == &hidden_window)
        },
    );
    server.tmux(&["kill-server"]);
    wait_for_presence_stream_end(&rx);
    drain.join().expect("drain thread");
}

/// The control-mode presence stream surfaces typed subscription changes — the
/// tmux fast path the elder sidebar consumes. Command changes and window closes
/// must produce typed control lines within the budget, and killing the server
/// must end the stream (`None`) rather than wedging it, so a dead watcher
/// degrades to the poll instead of a stuck frame.

#[test]
fn presence_watch_streams_typed_lines_and_ends_with_the_server() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("presence");
    let watch = rimz::mux::tmux::PresenceWatch::attach(&server.socket, "presence")
        .expect("attach control client");
    server.wait_for_control_client("presence");
    // Drain on a helper thread so the main thread owns the timeout. Initial
    // subscription values race with the first stimulus, so each assertion
    // filters for the line shape it caused.
    let (rx, drain) = spawn_presence_drain(watch);
    // `respawn-pane` drives a deterministic command-change subscription while
    // send-path coverage lives in
    // `headless_sends_work_with_no_client_and_presence_watch`.
    server.tmux(&["respawn-pane", "-k", "-t", "presence:0", "sleep 30"]);
    recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "sleep command change",
        |line| {
            matches!(
                line,
                rimz::mux::tmux::ControlLine::Subscription {
                    command: Some(command),
                    ..
                } if command == "sleep"
            )
        },
    );
    server.tmux(&["new-window", "-d", "-t", "presence", "-n", "gone", "sh"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "new window presence", |line| {
        matches!(line, rimz::mux::tmux::ControlLine::Nudge)
            || matches!(
                line,
                rimz::mux::tmux::ControlLine::Subscription {
                    command: Some(command),
                    ..
                } if command == "sh"
            )
    });
    server.tmux(&["kill-window", "-t", "presence:gone"]);
    recv_presence_line_until(&rx, Duration::from_secs(5), "window close", |line| {
        matches!(line, rimz::mux::tmux::ControlLine::WindowClosed { .. })
    });
    server.tmux(&["split-window", "-d", "-t", "presence:0", "sh"]);
    server.tmux(&["select-pane", "-t", "presence:0.0"]);
    let window_id = server.display("presence:0", "#{window_id}");
    let second_pane = server.display("presence:0.1", "#{pane_id}");
    server.tmux(&["select-pane", "-t", "presence:0.1"]);
    let line = recv_presence_line_until(
        &rx,
        Duration::from_secs(1),
        "active pane change notification",
        |line| {
            matches!(
                line,
                rimz::mux::tmux::ControlLine::WindowPaneChanged { pane, .. }
                    if pane == &second_pane
            )
        },
    );
    assert_eq!(
        line,
        rimz::mux::tmux::ControlLine::WindowPaneChanged {
            window: window_id,
            pane: second_pane,
        }
    );
    server.tmux(&["kill-server"]);
    wait_for_presence_stream_end(&rx);
    drain.join().expect("drain thread");
}

/// tmux 3.7 appends floating panes to every layout string of their window. The
/// reader must still surface the tiled layout rather than degrade each change in
/// that window to a nudge, and must keep the floating pane out of it.
#[test]
fn presence_watch_reads_layout_changes_beside_floating_panes() {
    require_tmux!();
    let server = TmuxServer::new();
    if !server.supports_floating_panes() {
        crate::common::skip("tmux predates 3.7 floating panes");
        return;
    }
    server.ensure_with_shell("floating-layout");
    let watch = rimz::mux::tmux::PresenceWatch::attach(&server.socket, "floating-layout")
        .expect("attach control client");
    server.wait_for_control_client("floating-layout");
    let (rx, drain) = spawn_presence_drain(watch);
    let window_id = server.display("floating-layout:0", "#{window_id}");
    let floating = server.stdout(&[
        "new-pane",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "floating-layout:0",
        "sleep",
        "120",
    ]);
    let split = server.stdout(&[
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        "floating-layout:0.0",
        "sleep",
        "120",
    ]);
    let line = recv_presence_line_until(
        &rx,
        Duration::from_secs(5),
        "layout change naming the split",
        |line| {
            matches!(
                line,
                rimz::mux::tmux::ControlLine::LayoutChange { panes, .. }
                    if panes.iter().any(|pane| pane.id == split)
            )
        },
    );
    let rimz::mux::tmux::ControlLine::LayoutChange { window, panes, .. } = line else {
        unreachable!("the matcher accepts only layout changes");
    };
    assert_eq!(window, window_id);
    assert!(
        panes.iter().all(|pane| pane.id != floating),
        "floating pane {floating} leaked into the tiled layout: {panes:?}",
    );
    server.tmux(&["kill-server"]);
    wait_for_presence_stream_end(&rx);
    drain.join().expect("drain thread");
}
