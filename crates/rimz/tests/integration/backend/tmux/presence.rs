#![allow(clippy::print_stdout, clippy::print_stderr)]

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
