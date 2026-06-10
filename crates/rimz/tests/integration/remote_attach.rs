//! `rimz remote connect`: the SSH launcher and its reconnect supervisor.
//!
//! No real ssh or host anywhere: the print form needs no binary at all, and
//! the exec form drives the `ssh-trace` shim through `RIMZ_SSH_BIN`,
//! asserting the exact argv handed to ssh and scripting link drops via
//! `$RIMZ_TEST_SSH_PLAN`. Quoting precision lives in `remote/mod.rs` unit tests;
//! these prove the CLI surface end to end.

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use crate::common::{CommandTimeoutExt, Env};

fn ssh_shim() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ssh-trace"))
}

/// The keepalive prefix shared by supervised and one-shot remote attaches.
const SSH_KEEPALIVES: [&str; 6] = [
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ConnectTimeout=10",
];

/// One `Vec<argv>` per shim invocation, from the tab-joined trace log.
fn shim_invocations(log: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(log)
        .expect("read ssh trace log")
        .lines()
        .filter(|line| !line.trim().is_empty())
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

fn write_link_notify_command_config(env: &Env) {
    let dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&dir).expect("mkdir rimz config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[notifications]\ncommand = '''printf '%s|%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" >> \"$RIMZ_NOTIFY_TEST_LOG\"'''\n",
    )
    .expect("write notify config");
}

fn wait_for_notify_log(path: &Path, needles: &[&str]) -> String {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if needles.iter().all(|needle| text.contains(needle)) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "notify log did not contain {needles:?}; saw:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn is_probe_invocation(argv: &[String]) -> bool {
    argv.iter()
        .any(|arg| arg.contains("remote link-stats ingest"))
}

fn is_control_check_invocation(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|args| args[0] == "-O" && args[1] == "check")
}

fn is_main_invocation(argv: &[String]) -> bool {
    !is_probe_invocation(argv) && !is_control_check_invocation(argv)
}

#[test]
fn link_stats_ingest_writes_the_runtime_sidecar_and_acks() {
    use std::io::Write as _;

    let env = Env::new();
    let dir = env.project_root.to_string_lossy().into_owned();
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .env("SSH_CONNECTION", "client-port server-port")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn link-stats ingest");
    let probe = serde_json::json!({
        "v": "rimz.link.v1",
        "seq": 7,
        "sent_at_ms": 1_000u64,
        "stats": {
            "rtt_ms": 42,
            "miss_pct": 3,
            "window": 12
        }
    });
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{probe}").expect("write probe");
    drop(stdin);
    let out = child.wait_with_output().expect("wait link-stats ingest");
    assert!(
        out.status.success(),
        "ingest succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ack: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ack json");
    assert_eq!(ack["v"], "rimz.link.v1");
    assert_eq!(ack["seq"], 7);

    let runtime = rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root)
        .expect("runtime paths");
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(rimz::remote::link::stats_path(&runtime)).unwrap())
            .expect("stats file json");
    assert_eq!(file["v"], "rimz.link.v1");
    assert_eq!(file["client"], "client-port server-port");
    assert_eq!(file["stats"]["rtt_ms"], 42);
    assert_eq!(file["stats"]["miss_pct"], 3);
}

#[test]
fn link_stats_ingest_schema_mismatch_exits_as_version_skew() {
    use std::io::Write as _;

    let env = Env::new();
    let dir = env.project_root.to_string_lossy().into_owned();
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn link-stats ingest");
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "v": "rimz.link.v999",
            "seq": 7,
            "sent_at_ms": 1_000u64,
            "stats": {
                "miss_pct": 0,
                "window": 0
            }
        })
    )
    .expect("write probe");
    drop(stdin);

    let out = child.wait_with_output().expect("wait link-stats ingest");

    assert_eq!(
        out.status.code(),
        Some(2),
        "schema mismatch is classified as probe version skew"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unsupported link probe schema"),
        "stderr names the mismatch: {stderr}"
    );
}

#[test]
fn exec_hands_ssh_the_expected_argv() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0 → clean exit\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert_eq!(invocations.len(), 1, "one ssh run");
    let argv = &invocations[0];
    assert_eq!(argv.len(), 15, "snippet is a single argv element: {argv:?}");
    assert!(argv[0].ends_with("ssh-trace"));
    assert_eq!(argv[1..7], SSH_KEEPALIVES);
    assert_eq!(argv[7], "-o");
    assert_eq!(argv[8], "ControlMaster=auto");
    assert_eq!(argv[9], "-o");
    assert!(argv[10].starts_with("ControlPath="), "{argv:?}");
    assert_eq!(argv[11], "-t");
    assert_eq!(argv[12], "--");
    assert_eq!(argv[13], "dev-box");
    assert!(argv[14].starts_with("PATH=\"$HOME/.cargo/bin"));
    assert!(argv[14].ends_with("exec rimz attach --attach -- 'query-engine'"));
}

#[test]
fn supervised_connect_starts_a_probe_stream() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "150")
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert!(
        invocations.iter().any(|argv| is_probe_invocation(argv)),
        "probe stream is spawned: {invocations:?}"
    );
    assert!(
        invocations
            .iter()
            .any(|argv| is_control_check_invocation(argv)),
        "probe waits on a ControlMaster readiness check: {invocations:?}"
    );
    let main = invocations
        .iter()
        .find(|argv| is_main_invocation(argv))
        .expect("main ssh invocation");
    assert!(
        main.iter().any(|arg| arg == "ControlMaster=auto"),
        "main ssh owns the control master: {main:?}"
    );
}

#[test]
fn probe_stream_waits_for_control_master_before_starting() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let fallback = env.project_root.join("probe-before-master");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_CONTROL_MASTER_READY_DELAY_MS", "100")
        .env("RIMZ_TEST_PROBE_BEFORE_MASTER", &fallback)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "220")
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fallback.exists(),
        "probe stream must not start before ControlMaster is ready"
    );
    let invocations = shim_invocations(&log);
    let main_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main ssh invocation");
    let probe_index = invocations
        .iter()
        .position(|argv| is_probe_invocation(argv))
        .expect("probe stream invocation");
    assert!(
        probe_index > main_index,
        "probe stream starts only after the main ssh begins: {invocations:?}"
    );
    assert!(
        invocations[..probe_index]
            .iter()
            .any(|argv| is_control_check_invocation(argv)),
        "readiness checks run before the probe stream: {invocations:?}"
    );
}

#[test]
fn probe_stream_respawn_rechecks_control_master() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let fallback = env.project_root.join("probe-before-master");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_PROBE_BEFORE_MASTER", &fallback)
        .env("RIMZ_TEST_PROBE_EXIT_AFTER_ACKS", "1")
        .env("RIMZ_TEST_REMOVE_CONTROL_MASTER_ON_PROBE_EXIT", &ready)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "1300")
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fallback.exists(),
        "probe stream respawn must not bypass the ControlMaster readiness check"
    );
    let invocations = shim_invocations(&log);
    let first_probe_index = invocations
        .iter()
        .position(|argv| is_probe_invocation(argv))
        .expect("first probe stream invocation");
    assert!(
        invocations[first_probe_index + 1..]
            .iter()
            .any(|argv| is_control_check_invocation(argv)),
        "respawn path rechecks ControlMaster after the first probe exits: {invocations:?}"
    );
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_probe_invocation(argv))
            .count(),
        1,
        "no second probe stream starts after the master marker disappears: {invocations:?}"
    );
}

#[test]
fn probe_kill_switch_suppresses_the_probe_stream() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "100")
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert!(
        invocations.iter().all(|argv| !is_probe_invocation(argv)),
        "probe stream disabled: {invocations:?}"
    );
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
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        // Gatetime 0: even the shim's instant session counts as established.
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_MS", "1")
        .bounded_output()
        .expect("run rimz remote connect --attach");
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
        !stderr.contains("restored"),
        "a retry verdict must not emit a false restored line: {stderr}"
    );
    assert!(
        stderr.contains("(attempt 1)"),
        "attempts number per outage, not per lifetime: {stderr}"
    );
}

#[test]
fn local_link_notify_command_receives_lost_and_restored_env() {
    let env = Env::new();
    write_link_notify_command_config(&env);
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let notify_log = env.project_root.join("notify.log");
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "80")
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .env("RIMZ_REMOTE_BACKOFF_MS", "1")
        .env("RIMZ_NOTIFY_TEST_LOG", &notify_log)
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "reconnect ends on clean detach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let text = wait_for_notify_log(
        &notify_log,
        &[
            "link_lost|Rimz: remote link lost|SSH to dev-box dropped; reconnecting.",
            "link_restored|Rimz: remote link restored|SSH to dev-box is responsive again.",
        ],
    );
    assert_eq!(
        text.matches("link_lost|").count(),
        1,
        "lost edge fires once:\n{text}"
    );
    assert_eq!(
        text.matches("link_restored|").count(),
        1,
        "restored edge fires once:\n{text}"
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
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(!out.status.success(), "transport failure surfaces");
    assert_eq!(shim_invocations(&log).len(), 1, "no retry");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("255"), "names the ssh exit: {stderr}");
}

#[test]
fn no_reconnect_hands_the_link_to_one_ssh() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = env
        .rimz()
        .args([
            "remote",
            "connect",
            "dev-box:query-engine",
            "--attach",
            "--no-reconnect",
        ])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .bounded_output()
        .expect("run rimz remote connect --no-reconnect");
    assert!(
        out.status.success(),
        "exec'd shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(shim_invocations(&log).len(), 1, "a single exec'd ssh");
}

#[test]
fn remote_alias_round_trip_connects_lists_resets_and_deletes() {
    let env = Env::new();
    let add = env
        .rimz()
        .args(["remote", "add", "prod", "agent@prod-box:query-engine"])
        .bounded_output()
        .expect("run rimz remote add");
    assert!(
        add.status.success(),
        "add succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&add.stderr),
    );
    let remote_file = env.config_root().join("rimz").join("remote.toml");
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(text.contains("name = \"prod\""), "{text}");
    assert!(
        text.contains("target = \"agent@prod-box:query-engine\""),
        "{text}"
    );

    let printed = env
        .rimz()
        .args(["remote", "connect", "prod", "--print"])
        .bounded_output()
        .expect("run rimz remote connect alias --print");
    let line = stdout_line(&printed);
    assert!(
        line.contains("agent@prod-box"),
        "alias target rides into ssh: {line}"
    );
    assert!(
        line.contains("query-engine"),
        "alias session rides into remote rimz: {line}"
    );

    let list = env
        .rimz()
        .args(["remote", "list", "--json"])
        .bounded_output()
        .expect("run rimz remote list --json");
    assert!(
        list.status.success(),
        "list succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&list.stderr),
    );
    let json: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("remote list json parses");
    assert_eq!(json["remotes"][0]["name"], "prod");
    assert_eq!(json["remotes"][0]["reconnect"], true);

    let reset = env
        .rimz()
        .args(["remote", "reset", "prod", "--print"])
        .bounded_output()
        .expect("run rimz remote reset --print");
    let line = stdout_line(&reset);
    assert!(
        line.contains("--no-resume"),
        "remote reset injects --no-resume: {line}"
    );

    let del = env
        .rimz()
        .args(["remote", "del", "prod"])
        .bounded_output()
        .expect("run rimz remote del");
    assert!(
        del.status.success(),
        "delete succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&del.stderr),
    );
}

#[test]
fn remote_add_persists_only_remote_scoped_mux_flags() {
    let env = Env::new();
    let add_global = env
        .rimz()
        .args([
            "--mux",
            "tmux",
            "remote",
            "add",
            "global",
            "global-box:query-engine",
        ])
        .bounded_output()
        .expect("run rimz --mux tmux remote add");
    assert!(
        add_global.status.success(),
        "global add succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&add_global.stderr),
    );
    let remote_file = env.config_root().join("rimz").join("remote.toml");
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(text.contains("name = \"global\""), "{text}");
    assert!(
        !text.contains("mux ="),
        "global --mux is a per-invocation override, not persisted alias state: {text}",
    );

    let add_remote_scoped = env
        .rimz()
        .args([
            "remote",
            "--mux",
            "tmux",
            "add",
            "scoped",
            "scoped-box:query-engine",
        ])
        .bounded_output()
        .expect("run rimz remote --mux tmux add");
    assert!(
        add_remote_scoped.status.success(),
        "remote-scoped add succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&add_remote_scoped.stderr),
    );
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(text.contains("name = \"scoped\""), "{text}");
    assert_eq!(
        text.matches("mux = \"tmux\"").count(),
        1,
        "remote-scoped --mux pins the alias mux: {text}",
    );

    let add_local = env
        .rimz()
        .args([
            "remote",
            "add",
            "local",
            "local-box:query-engine",
            "--mux",
            "tmux",
        ])
        .bounded_output()
        .expect("run rimz remote add --mux");
    assert!(
        add_local.status.success(),
        "local add succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&add_local.stderr),
    );
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(text.contains("name = \"local\""), "{text}");
    assert_eq!(
        text.matches("mux = \"tmux\"").count(),
        2,
        "local --mux pins the alias mux: {text}",
    );
}
