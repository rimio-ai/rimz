//! `rimz attach --remote`: the SSH launcher and its reconnect supervisor.
//!
//! No real ssh or host anywhere: the print form needs no binary at all, and
//! the exec form drives the `ssh-trace` shim through `RIMZ_SSH_BIN`,
//! asserting the exact argv handed to ssh and scripting link drops via
//! `$RIMZ_TEST_SSH_PLAN`. Quoting precision lives in `remote.rs` unit tests;
//! these prove the CLI surface end to end.

use std::path::{Path, PathBuf};
use std::process::Output;

use crate::common::{CommandTimeoutExt, Env};

fn ssh_shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssh-trace"))
}

/// The fixed option ladder every remote attach carries, argv elements 1..=8.
const SSH_LADDER: [&str; 8] = [
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ConnectTimeout=10",
    "-t",
    "--",
];

/// One `Vec<argv>` per shim invocation, from the tab-joined trace log.
fn shim_invocations(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .expect("read ssh trace log")
        .lines()
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect())
        .collect()
}

fn stdout_line(out: &Output) -> String {
    assert!(
        out.status.success(),
        "expected success\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn print_form_emits_the_full_ssh_command() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--print"])
        .bounded_output()
        .expect("run rimz attach --remote --print");
    let line = stdout_line(&out);
    assert!(
        line.starts_with("ssh -o ServerAliveInterval=5 -o ServerAliveCountMax=3"),
        "keepalive ladder leads the command: {line}"
    );
    assert!(
        line.contains(" -t -- dev-box '"),
        "snippet is quoted: {line}"
    );
    assert!(
        line.contains("rimz attach --attach"),
        "session form reattaches: {line}"
    );
    assert!(
        line.contains("query-engine"),
        "target rides the snippet: {line}"
    );
    assert!(
        line.contains("command -v rimz"),
        "snippet guards the remote rimz: {line}"
    );
}

#[test]
fn path_form_starts_the_remote_room() {
    let env = Env::new();
    let out = env
        .rimz()
        .args([
            "attach",
            "--remote",
            "dev-box:~/code/query-engine",
            "--print",
        ])
        .bounded_output()
        .expect("run rimz attach --remote --print");
    let line = stdout_line(&out);
    assert!(
        line.contains("rimz start --attach"),
        "path form births the room: {line}"
    );
    assert!(line.contains("$HOME"), "tilde expands remotely: {line}");
    assert!(
        !line.contains("rimz attach --attach"),
        "not the session form: {line}"
    );
}

#[test]
fn auto_mode_without_a_tty_prints() {
    let env = Env::new();
    // `bounded_output` pipes stdio, so Auto resolves to Print — the
    // testing.md attach invariant, unchanged for remote targets.
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine"])
        .bounded_output()
        .expect("run rimz attach --remote");
    let line = stdout_line(&out);
    assert!(line.starts_with("ssh "), "non-TTY auto prints: {line}");
}

#[test]
fn print_form_needs_no_ssh_binary() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--print"])
        .env("PATH", "")
        .bounded_output()
        .expect("run rimz attach --remote --print");
    assert!(
        out.status.success(),
        "print never resolves ssh: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn exec_hands_ssh_the_expected_argv() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .bounded_output()
        .expect("run rimz attach --remote --attach");
    assert!(
        out.status.success(),
        "shim exits 0 → clean exit\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert_eq!(invocations.len(), 1, "one ssh run");
    let argv = &invocations[0];
    assert_eq!(argv.len(), 11, "snippet is a single argv element: {argv:?}");
    assert!(argv[0].ends_with("ssh-trace"));
    assert_eq!(argv[1..9], SSH_LADDER);
    assert_eq!(argv[9], "dev-box");
    assert!(argv[10].starts_with("PATH=\"$HOME/.cargo/bin"));
    assert!(argv[10].ends_with("exec rimz attach --attach -- 'query-engine'"));
}

#[test]
fn link_drop_on_an_established_session_reconnects() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    // First session drops the link (255), the reattach detaches cleanly (0).
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        // Gatetime 0: even the shim's instant session counts as established.
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_MS", "1")
        .bounded_output()
        .expect("run rimz attach --remote --attach");
    assert!(
        out.status.success(),
        "reconnect ends on the clean detach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        shim_invocations(&log).len(),
        2,
        "dropped once, reattached once"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reconnecting"),
        "the supervisor narrates the retry: {stderr}"
    );
    assert!(
        stderr.contains("(attempt 1)"),
        "attempts number per outage, not per lifetime: {stderr}"
    );
}

#[test]
fn first_connection_failure_never_retries() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n").expect("write plan");
    // Default gatetime: the shim's instant 255 reads as an auth/host
    // failure, not a drop — fatal, no password-prompt loop.
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .bounded_output()
        .expect("run rimz attach --remote --attach");
    assert!(!out.status.success(), "transport failure surfaces");
    assert_eq!(shim_invocations(&log).len(), 1, "no retry");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("255"), "names the ssh exit: {stderr}");
}

#[test]
fn missing_remote_rimz_is_fatal() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "127\n").expect("write plan");
    let out = env
        .rimz()
        .args(["attach", "--remote", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .bounded_output()
        .expect("run rimz attach --remote --attach");
    assert!(
        !out.status.success(),
        "missing remote rimz is not a link drop"
    );
    assert_eq!(
        shim_invocations(&log).len(),
        1,
        "retrying cannot install rimz"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("127"), "names the exit: {stderr}");
}

#[test]
fn no_reconnect_hands_the_link_to_one_ssh() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args([
            "attach",
            "--remote",
            "dev-box:query-engine",
            "--attach",
            "--no-reconnect",
        ])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .bounded_output()
        .expect("run rimz attach --remote --no-reconnect");
    assert!(
        out.status.success(),
        "exec'd shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(shim_invocations(&log).len(), 1, "a single exec'd ssh");
}

#[test]
fn no_resume_and_mux_ride_the_remote_invocation() {
    let env = Env::new();
    let out = env
        .rimz()
        .args([
            "attach",
            "--remote",
            "dev-box:query-engine",
            "--no-resume",
            "--mux",
            "tmux",
            "--print",
        ])
        .bounded_output()
        .expect("run rimz attach --remote --print");
    let line = stdout_line(&out);
    assert!(
        line.contains("--no-resume --mux tmux"),
        "local flags ride into the remote rimz: {line}"
    );
}

#[test]
fn remote_conflicts_with_a_positional_session() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["attach", "query-engine", "--remote", "dev-box:other"])
        .bounded_output()
        .expect("run rimz attach SESSION --remote");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "clap rejects the combination: {stderr}"
    );
}

#[test]
fn no_reconnect_requires_remote() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["attach", "--no-reconnect"])
        .bounded_output()
        .expect("run rimz attach --no-reconnect");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--remote"),
        "names the missing flag: {stderr}"
    );
}

#[test]
fn malformed_targets_fail_with_the_fix() {
    let env = Env::new();
    let cases = [
        ("dev-box", "missing the"),
        ("dev-box:", "nothing after"),
        (":query-engine", "empty host"),
        ("[::1:query-engine", "unclosed"),
        ("dev-box:~alice/code", "absolute path"),
    ];
    for (target, needle) in cases {
        let out = env
            .rimz()
            .args(["attach", "--remote", target, "--print"])
            .bounded_output()
            .expect("run rimz attach --remote");
        assert!(!out.status.success(), "`{target}` must not parse");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(needle),
            "`{target}` names the fix (`{needle}`): {stderr}"
        );
    }
}
