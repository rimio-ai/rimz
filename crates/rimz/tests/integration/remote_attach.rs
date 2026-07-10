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
    crate::common::cargo_bin("ssh-trace", env!("CARGO_BIN_EXE_ssh-trace"))
}

/// The transport options shared by supervised and one-shot remote attaches.
const SSH_TRANSPORT_OPTS: [&str; 8] = [
    "-o",
    "ServerAliveInterval=5",
    "-o",
    "ServerAliveCountMax=3",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "Compression=yes",
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

fn write_infocmp_shim(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nprintf 'CANNED,'\n").expect("write infocmp shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod infocmp shim");
    }
}

enum InfocmpFixture {
    Ambient,
    Missing,
    Copy,
}

fn run_exec_with_term(term: &str, colorterm: Option<&str>, infocmp: InfocmpFixture) -> Vec<String> {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = env.rimz();
    cmd.args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .env("TERM", term);
    match colorterm {
        Some(value) => {
            cmd.env("COLORTERM", value);
        }
        None => {
            cmd.env_remove("COLORTERM");
        }
    }
    match infocmp {
        InfocmpFixture::Ambient => {}
        InfocmpFixture::Missing => {
            cmd.env("RIMZ_INFOCMP_BIN", env.project_root.join("missing-infocmp"));
        }
        InfocmpFixture::Copy => {
            let infocmp = env.project_root.join("infocmp-shim");
            write_infocmp_shim(&infocmp);
            cmd.env("RIMZ_INFOCMP_BIN", infocmp);
        }
    }
    let out = cmd
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0 -> clean exit\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert_eq!(invocations.len(), 1, "one ssh run");
    let argv = invocations.into_iter().next().expect("ssh invocation");
    assert_eq!(argv.len(), 17, "snippet is a single argv element: {argv:?}");
    assert!(argv[0].ends_with("ssh-trace"));
    assert_eq!(argv[1..9], SSH_TRANSPORT_OPTS);
    assert_eq!(argv[9], "-o");
    assert_eq!(argv[10], "ControlMaster=auto");
    assert_eq!(argv[11], "-o");
    assert!(argv[12].starts_with("ControlPath="), "{argv:?}");
    assert_eq!(argv[13], "-t");
    assert_eq!(argv[14], "--");
    assert_eq!(argv[15], "dev-box");
    argv
}

fn snippet(argv: &[String]) -> &str {
    argv.last().expect("snippet")
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
    use std::io::{BufRead as _, Write as _};

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
    let stdout = child.stdout.take().expect("stdout");
    let mut stdout = std::io::BufReader::new(stdout);
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{probe}").expect("write probe");
    let mut ack = String::new();
    stdout.read_line(&mut ack).expect("read link ack");
    let ack: serde_json::Value = serde_json::from_str(&ack).expect("ack json");
    assert_eq!(ack["v"], "rimz.link.v1");
    assert_eq!(ack["seq"], 7);

    let runtime = rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root)
        .expect("runtime paths");
    let path = rimz::remote::link::stats_path(&runtime);
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("stats file json");
    assert_eq!(file["v"], "rimz.link.v1");
    assert_eq!(file["client"], "client-port server-port");
    assert_eq!(file["stats"]["rtt_ms"], 42);
    assert_eq!(file["stats"]["miss_pct"], 3);

    drop(stdin);
    let out = child.wait_with_output().expect("wait link-stats ingest");
    assert!(
        out.status.success(),
        "ingest succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!path.exists(), "clean stream end removes its sidecar");
}

#[test]
fn link_stats_ingest_keeps_a_newer_publishers_sidecar() {
    let env = Env::new();
    let runtime = rimz::RuntimePaths::under(env.workspace_id.clone(), &env.runtime_root)
        .expect("runtime paths");
    let path = rimz::remote::link::stats_path(&runtime);
    let seeded = rimz::remote::link::LinkStatsFile::new(
        1_000,
        "new-client-port new-server-port".to_owned(),
        rimz::remote::link::LinkStats::default(),
    );
    rimz::store::atomic::write_temp_then_rename_cache(&path, &seeded)
        .expect("seed newer link stats");

    let dir = env.project_root.to_string_lossy().into_owned();
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .env("SSH_CONNECTION", "old-client-port old-server-port")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn link-stats ingest");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait link-stats ingest");
    assert!(
        out.status.success(),
        "ingest succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let remaining: rimz::remote::link::LinkStatsFile =
        serde_json::from_slice(&std::fs::read(path).expect("read seeded stats"))
            .expect("parse seeded stats");
    assert_eq!(remaining, seeded);
}

#[test]
fn exec_uses_ssh_shim_and_applies_terminal_plan() {
    let portable = run_exec_with_term("xterm-256color", None, InfocmpFixture::Ambient);
    assert!(snippet(&portable).starts_with("PATH=\"$HOME/.cargo/bin"));
    assert!(snippet(&portable).ends_with("exec rimz attach --attach -- 'query-engine'"));
    assert!(
        !snippet(&portable).contains("COLORTERM"),
        "{}",
        snippet(&portable)
    );

    let truecolor =
        run_exec_with_term("xterm-256color", Some("truecolor"), InfocmpFixture::Ambient);
    assert!(
        snippet(&truecolor).contains("export COLORTERM=truecolor; exec rimz"),
        "{}",
        snippet(&truecolor)
    );

    let downgrade = run_exec_with_term("alacritty", None, InfocmpFixture::Missing);
    assert!(
        snippet(&downgrade).contains("export TERM=xterm-256color; exec rimz"),
        "{}",
        snippet(&downgrade)
    );

    let copy = run_exec_with_term("alacritty", None, InfocmpFixture::Copy);
    assert!(
        snippet(&copy)
            .contains("printf '%s\\n' 'CANNED,' | tic -x - 2>/dev/null && export TERM='alacritty'"),
        "{}",
        snippet(&copy)
    );
    assert!(snippet(&copy).ends_with("exec rimz attach --attach -- 'query-engine'"));
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
        .env("TERM", "xterm-256color")
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
        .env("TERM", "xterm-256color")
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
        .env("TERM", "xterm-256color")
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
fn remote_alias_cli_lifecycle_covers_add_update_list_reset_rm() {
    let env = Env::new();
    let remote_file = env.config_root().join("rimz").join("remote.toml");

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
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(text.contains("name = \"prod\""), "{text}");
    assert!(
        text.contains("target = \"agent@prod-box:query-engine\""),
        "{text}"
    );

    let dup = env
        .rimz()
        .args(["remote", "add", "prod", "other-box:other-engine"])
        .bounded_output()
        .expect("run duplicate rimz remote add");
    assert!(
        !dup.status.success(),
        "duplicate add must fail non-interactively"
    );
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(
        text.contains("agent@prod-box:query-engine"),
        "target unchanged: {text}",
    );

    let update = env
        .rimz()
        .args(["remote", "update", "prod", "agent@prod-box:other-engine"])
        .bounded_output()
        .expect("run rimz remote update");
    assert!(
        update.status.success(),
        "update succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&update.stderr),
    );
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(
        text.contains("agent@prod-box:other-engine"),
        "target replaced: {text}",
    );

    let printed = env
        .rimz()
        .args(["remote", "connect", "prod", "--print"])
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("run rimz remote connect alias --print");
    let line = stdout_line(&printed);
    assert!(
        line.contains("agent@prod-box"),
        "alias target rides into ssh: {line}"
    );
    assert!(
        line.contains("other-engine"),
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
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("run rimz remote reset --print");
    let line = stdout_line(&reset);
    assert!(
        line.contains("--no-resume"),
        "remote reset injects --no-resume: {line}"
    );

    let missing = env
        .rimz()
        .args(["remote", "update", "nope", "host:path"])
        .bounded_output()
        .expect("run missing rimz remote update");
    assert!(
        !missing.status.success(),
        "update of absent alias must fail"
    );

    let rm = env
        .rimz()
        .args(["remote", "rm", "prod"])
        .bounded_output()
        .expect("run rimz remote rm");
    assert!(
        rm.status.success(),
        "rm succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&rm.stderr),
    );
    let text = std::fs::read_to_string(&remote_file).expect("read remote.toml");
    assert!(
        !text.contains("name = \"prod\""),
        "alias removed from file: {text}"
    );
}
