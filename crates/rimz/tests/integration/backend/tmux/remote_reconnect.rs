//! Live reconnect coverage with a real query-producing tmux client.

use std::time::{Duration, Instant};

use crate::common::Env;
use crate::common::ssh_trace::{
    Answers, Counts, FakeTerminal, main_invocation_count, remote_connect_pty,
    remote_connect_pty_command,
};

use super::support::*;

#[test]
fn supervised_reconnect_keeps_duplicate_terminal_replies_out_of_the_tmux_pane() {
    require_tmux!();

    let server = TmuxServer::new();
    let session = "reconnect";
    server.ensure_with_shell(session);
    let pane = list_session_panes(&server, session)[0].pane_id.clone();

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let exit_plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&exit_plan, "255\n0\n").expect("write ssh plan");

    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    let exec_argv = format!(
        "tmux\t-S\t{}\tattach\t-t\t{session}",
        server.socket.display()
    );
    cmd.env("RIMZ_TEST_SSH_PLAN", &exit_plan);
    cmd.env("RIMZ_TEST_SSH_EXEC_ARGV", exec_argv);
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    drop(pair.slave);
    let terminal = FakeTerminal::new(pair.master, Duration::from_millis(600), Answers::Ghostty);
    let deadline = Instant::now() + Duration::from_secs(10);

    wait_for_queries(&terminal, deadline, |counts| counts.xtversion >= 1);
    let client_tty = server.stdout(&["list-clients", "-t", session, "-F", "#{client_tty}"]);
    assert!(!client_tty.is_empty(), "first tmux client has a tty");
    server.tmux(&["detach-client", "-t", &client_tty]);

    wait_for_count(&log, deadline, 2);
    wait_for_queries(&terminal, deadline, |counts| counts.xtversion >= 2);
    std::thread::sleep(Duration::from_millis(700));
    terminal.write(b"printf 'RIMZ-%s\\n' MARKER\r");

    let capture = capture_pane_until(
        &server.backend,
        &pane,
        "RIMZ-MARKER",
        Duration::from_secs(5),
    );
    assert!(
        capture.contains("RIMZ-MARKER"),
        "user input did not reach the replacement tmux client; pane:\n{capture}\npty:\n{}",
        String::from_utf8_lossy(&terminal.output())
    );
    for stale_token in ["62;22;52", "1;10;0c", "ghostty"] {
        assert!(
            !capture.contains(stale_token),
            "duplicate terminal reply leaked into pane: {stale_token:?}\npane:\n{capture}\npty:\n{}",
            String::from_utf8_lossy(&terminal.output())
        );
    }
    assert_eq!(
        terminal.queries_seen(),
        Counts {
            status: 1,
            da1: 2,
            da2: 2,
            xtversion: 2,
        },
        "two real tmux query sets plus one RimZ fence"
    );

    server.tmux(&["kill-server"]);
    let exit_deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= exit_deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let Some(status) = status else {
        let output = String::from_utf8_lossy(&terminal.output()).into_owned();
        drop(terminal);
        panic!("remote connect timed out; output:\n{output}");
    };
    let output = String::from_utf8_lossy(&terminal.finish()).into_owned();
    assert!(
        status.success(),
        "remote connect failed with {status:?}; output:\n{output}"
    );
}

fn wait_for_queries(terminal: &FakeTerminal, deadline: Instant, ready: impl Fn(Counts) -> bool) {
    while !ready(terminal.queries_seen()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready(terminal.queries_seen()),
        "terminal queries not seen before deadline: {:?}\npty:\n{}",
        terminal.queries_seen(),
        String::from_utf8_lossy(&terminal.output())
    );
}

fn wait_for_count(log: &std::path::Path, deadline: Instant, count: usize) {
    while main_invocation_count(log) < count && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        main_invocation_count(log),
        count,
        "visible attach count did not reach {count}"
    );
}
