//! `rimz remote connect`: the SSH launcher and its reconnect supervisor.
//!
//! No real ssh or host anywhere: the print form needs no binary at all, and
//! the exec form drives the `ssh-trace` shim through `RIMZ_SSH_BIN`,
//! asserting the exact argv handed to ssh and scripting link drops via
//! `$RIMZ_TEST_SSH_PLAN`. Quoting precision lives in `remote/mod.rs` unit tests;
//! these prove the CLI surface end to end.

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use nix::sys::termios::{self, InputFlags, LocalFlags, OutputFlags, Termios};
use portable_pty::{CommandBuilder, PtyPair, PtySize, native_pty_system};

use crate::common::{CommandTimeoutExt, Env};

fn ssh_shim() -> PathBuf {
    crate::common::cargo_bin("ssh-trace", env!("CARGO_BIN_EXE_ssh-trace"))
}

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
    Missing,
    Copy,
}

fn remote_connect_command(env: &Env, log: &Path) -> Command {
    let mut cmd = env.rimz();
    cmd.args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", log)
        .env("RIMZ_REMOTE_DIAL_MS", "0")
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1")
        .env("RIMZ_REMOTE_MIN_DISPLAY_MS", "0")
        .env("TERM", "xterm-256color");
    cmd
}

fn remote_connect_pty_command(env: &Env, log: &Path) -> CommandBuilder {
    let mut cmd = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut cmd);
    cmd.args(["remote", "connect", "dev-box:query-engine", "--attach"]);
    cmd.cwd(env.project_root.as_os_str());
    cmd.env("RIMZ_SSH_BIN", ssh_shim());
    cmd.env("RIMZ_TEST_SSH_LOG", log);
    cmd.env("RIMZ_REMOTE_DIAL_MS", "0");
    cmd.env("RIMZ_REMOTE_PROBE_MS", "0");
    cmd.env("RIMZ_REMOTE_INTERNET_PROBE", "0");
    cmd.env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "0");
    cmd.env("TERM", "xterm-256color");
    cmd
}

fn remote_connect_pty() -> PtyPair {
    native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open remote connect pty")
}

fn put_pty_in_raw_mode(pair: &PtyPair) {
    let mut cmd = CommandBuilder::new("stty");
    cmd.args(["raw", "-echo"]);
    let status = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn stty raw")
        .wait()
        .expect("wait stty raw");
    assert!(status.success(), "stty raw failed: {status:?}");
}

fn read_pty_termios(pair: &PtyPair) -> Termios {
    read_tty_termios(&pair.master.tty_name().expect("remote connect pty name"))
}

fn read_tty_termios(path: &Path) -> Termios {
    let tty = std::fs::File::open(path).expect("open remote connect pty");
    termios::tcgetattr(&tty).expect("read remote connect pty state")
}

fn run_pty_command(pair: PtyPair, cmd: CommandBuilder) -> (String, Termios) {
    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    let tty_name = pair.master.tty_name().expect("remote connect pty name");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut output = Vec::new();
        let _ = reader.read_to_end(&mut output);
        output
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let settings = read_tty_termios(&tty_name);
    drop(pair.master);
    let output =
        String::from_utf8_lossy(&reader_thread.join().expect("join pty reader")).into_owned();
    let status = status.unwrap_or_else(|| panic!("remote connect timed out; output:\n{output}"));
    assert!(
        status.success(),
        "remote connect failed with {status:?}; output:\n{output}"
    );
    (output, settings)
}

fn assert_shell_tty(settings: &Termios) {
    assert!(
        settings.input_flags.contains(InputFlags::ICRNL),
        "ICRNL missing: {settings:?}"
    );
    assert!(
        settings
            .output_flags
            .contains(OutputFlags::OPOST | OutputFlags::ONLCR),
        "OPOST/ONLCR missing: {settings:?}"
    );
    assert!(
        settings
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ISIG | LocalFlags::ECHO),
        "ICANON/ISIG/ECHO missing: {settings:?}"
    );
}

fn run_exec_with_term(colorterm: Option<&str>, infocmp: InfocmpFixture) -> Vec<String> {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = remote_connect_command(&env, &log);
    cmd.env("TERM", "alacritty");
    match colorterm {
        Some(value) => {
            cmd.env("COLORTERM", value);
        }
        None => {
            cmd.env_remove("COLORTERM");
        }
    }
    match infocmp {
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
    let master_index = invocations
        .iter()
        .position(|argv| is_master_invocation(argv))
        .expect("initial master invocation");
    let main_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main ssh invocation");
    assert!(
        master_index < main_index,
        "the confirmed master precedes the main attach: {invocations:?}"
    );
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_main_invocation(argv))
            .count(),
        1,
        "one main ssh run: {invocations:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("attaching to"),
        "the panel owns supervised connection presentation"
    );
    invocations[main_index].clone()
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

fn is_config_query_invocation(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "-G")
}

fn is_master_invocation(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "-M")
}

fn is_main_invocation(argv: &[String]) -> bool {
    !is_probe_invocation(argv)
        && !is_control_check_invocation(argv)
        && !is_config_query_invocation(argv)
        && !is_master_invocation(argv)
}

fn main_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect::<Vec<_>>())
        .filter(|argv| is_main_invocation(argv))
        .count()
}

fn master_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split('\t').map(ToOwned::to_owned).collect::<Vec<_>>())
        .filter(|argv| is_master_invocation(argv))
        .count()
}

fn tunnel_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split('\t').any(|arg| arg == "-L"))
        .count()
}

fn web_prep_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("rimz web open"))
        .count()
}

fn reserve_local_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve local web port");
    listener.local_addr().expect("reserved address").port()
}

fn closed_ssh_endpoint(env: &Env) -> (PathBuf, SocketAddr) {
    let path = env.project_root.join("ssh-config.txt");
    let reservation = TcpListener::bind(("127.0.0.1", 0)).expect("reserve dial port");
    let address = reservation.local_addr().expect("dial address");
    drop(reservation);
    std::fs::write(
        &path,
        format!("hostname 127.0.0.1\nport {}\n", address.port()),
    )
    .expect("write ssh config fixture");
    (path, address)
}

fn http_204_probe() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind HTTP probe");
    listener
        .set_nonblocking(true)
        .expect("make HTTP probe nonblocking");
    let address = listener.local_addr().expect("HTTP probe address");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0; 1024];
                    let _ = stream.read(&mut request);
                    let _ =
                        stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    });
    (format!("http://{address}/generate_204"), stop, join)
}

fn remote_web_command(env: &Env, log: &Path, port: u16) -> Command {
    let mut cmd = remote_connect_command(env, log);
    cmd.args(["--web", "--web-port", &port.to_string()]);
    cmd
}

fn write_browser_shim(env: &Env) -> (PathBuf, PathBuf) {
    let bin = env.project_root.join("browser-bin");
    let log = env.project_root.join("browser.log");
    std::fs::create_dir_all(&bin).expect("create browser bin");
    let opener = bin.join("xdg-open");
    std::fs::write(
        &opener,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$RIMZ_TEST_BROWSER_LOG\"\n",
    )
    .expect("write browser shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755))
            .expect("chmod browser shim");
    }
    (bin, log)
}

fn wait_for_main_invocations(
    child: &mut std::process::Child,
    log: &Path,
    expected: usize,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let count = main_invocation_count(log);
        if count >= expected {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let trace = std::fs::read_to_string(log).unwrap_or_default();
            panic!(
                "expected {expected} main ssh invocations within {timeout:?}; saw {count}\ntrace:\n{trace}\nstderr:\n{stderr}"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_tunnel_invocation(child: &mut std::process::Child, log: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while tunnel_invocation_count(log) == 0 {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "web tunnel did not start; trace={:?}",
                shim_invocations(log)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn link_stats_ingest_writes_the_runtime_sidecar_and_acks() {
    use std::io::{BufRead as _, Write as _};

    let env = Env::new();
    let dir = env.project_root.to_string_lossy().into_owned();
    let proc_net = env.project_root.join("proc-net");
    std::fs::create_dir_all(&proc_net).expect("create proc net fixture");
    let uid = nix::unistd::getuid().as_raw();
    let row = |local: &str, state: &str, row_uid: u32| {
        format!(
            "0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 {row_uid} 0 1"
        )
    };
    std::fs::write(
        proc_net.join("tcp"),
        [
            row("0100007F:0BB8", "0A", uid),
            row("0100007F:0050", "0A", uid),
            row("0100007F:0FA0", "0A", uid.saturating_add(1)),
            row("0200000A:1388", "0A", uid),
        ]
        .join("\n"),
    )
    .expect("write tcp fixture");
    std::fs::write(
        proc_net.join("tcp6"),
        row("00000000000000000000000000000000:1F90", "0A", uid),
    )
    .expect("write tcp6 fixture");
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .env("SSH_CONNECTION", "client-port server-port")
        .env("RIMZ_PROC_NET_DIR", &proc_net)
        .env("RIMZ_PORTS_SWEEP_MS", "0")
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
    assert_eq!(ack["ports"], serde_json::json!([3000, 8080]));

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
fn supervised_connect_opens_new_remote_listener_forwards() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let port = reserve_local_port();
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .bounded_output()
        .expect("run auto-forward connect");
    assert!(
        out.status.success(),
        "auto-forward connect succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let invocations = shim_invocations(&log);
    let forwarding = format!("127.0.0.1:{port}:localhost:{port}");
    assert!(
        invocations.iter().any(|argv| {
            argv.windows(2)
                .any(|args| args[0] == "-O" && args[1] == "forward")
                && argv
                    .windows(2)
                    .any(|args| args[0] == "-L" && args[1] == forwarding)
        }),
        "forward control call missing from {invocations:?}"
    );
}

#[test]
fn exec_downgrades_or_copies_terminal_at_the_cli_boundary() {
    let downgrade = run_exec_with_term(None, InfocmpFixture::Missing);
    assert!(
        snippet(&downgrade).contains("export TERM=xterm-256color; exec rimz"),
        "{}",
        snippet(&downgrade)
    );

    let copy = run_exec_with_term(Some("truecolor"), InfocmpFixture::Copy);
    assert!(
        snippet(&copy)
            .contains("printf '%s\\n' 'CANNED,' | tic -x - 2>/dev/null && export TERM='alacritty'"),
        "{}",
        snippet(&copy)
    );
    assert!(
        snippet(&copy).contains("export COLORTERM=truecolor;"),
        "{}",
        snippet(&copy)
    );
}

#[test]
fn one_shot_connect_repairs_a_raw_tty_before_exec() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    put_pty_in_raw_mode(&pair);
    let damaged = read_pty_termios(&pair);
    assert!(rimz::remote::tty::termios_damaged(
        damaged.input_flags,
        damaged.output_flags,
        damaged.local_flags
    ));

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.arg("--no-reconnect");
    let (output, settings) = run_pty_command(pair, cmd);

    assert_shell_tty(&settings);
    assert!(
        !output.contains("attaching to"),
        "one-shot exec leaves presentation to ssh: {output}"
    );
}

#[test]
fn supervised_initial_auth_failure_falls_back_to_one_interactive_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    std::fs::write(&plan, "2\n").expect("write main plan");
    std::fs::write(&master_plan, "255\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "Permission denied (publickey).\n",
        )
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        !out.status.success(),
        "the foreground failure remains fatal"
    );
    assert_eq!(master_invocation_count(&log), 1);
    assert_eq!(main_invocation_count(&log), 1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("connect to dev-box failed — Permission denied (publickey)."),
        "{stderr}"
    );
    assert!(
        stderr.contains("ssh to dev-box exited with status 2; not reconnecting"),
        "{stderr}"
    );
}

#[test]
fn supervised_initial_transport_failure_retries_before_main_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let master_plan = env.project_root.join("ssh-master.plan");
    std::fs::write(&master_plan, "255\n0\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        out.status.success(),
        "transport retry reaches the main attach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    let main_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main attach");
    assert_eq!(
        invocations[..main_index]
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        2,
        "both masters precede the main attach: {invocations:?}"
    );
}

#[test]
fn supervised_master_deadline_kills_and_repaces_a_hung_attempt() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let master_plan = env.project_root.join("ssh-master.plan");
    let ready_plan = env.project_root.join("ssh-master-ready.plan");
    std::fs::write(&master_plan, "0\n255\n").expect("write master plan");
    std::fs::write(&ready_plan, "0\n").expect("write ready plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env("RIMZ_TEST_SSH_MASTER_READY_PLAN", &ready_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "Permission denied (publickey).\n",
        )
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_REMOTE_MASTER_TIMEOUT_MS", "50")
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        out.status.success(),
        "the replacement attempt reaches interactive fallback\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("SSH connect timed out after 0s"),
        "the attempt timeout is visible"
    );
}

#[test]
fn tun_route_skips_dead_tcp_checkpoint_and_keeps_ssh_retries_fast() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let master_plan = env.project_root.join("ssh-master.plan");
    let (ssh_config, _) = closed_ssh_endpoint(&env);
    std::fs::write(&master_plan, "255\n0\n").expect("write master plan");

    let started = Instant::now();
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .env("RIMZ_REMOTE_DIAL_MS", "10")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "20")
        .env("RIMZ_REMOTE_TUN", "utun3")
        .bounded_output()
        .expect("run TUN-routed remote connect");

    assert!(out.status.success());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "TUN bypass keeps the retry on reachable pacing"
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("server route to dev-box uses TUN utun3 — TCP check skipped"),
        "the skipped checkpoint is visible"
    );
}

#[test]
fn supervised_connect_restores_tty_and_resets_emulator_after_retry() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let tty_state_log = env.project_root.join("ssh-tty-state.log");
    std::fs::write(&plan, "255\n0\n").expect("write ssh plan");
    let pair = remote_connect_pty();
    put_pty_in_raw_mode(&pair);
    let damaged = read_pty_termios(&pair);
    assert!(rimz::remote::tty::termios_damaged(
        damaged.input_flags,
        damaged.output_flags,
        damaged.local_flags
    ));

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_TTY_STATE_LOG", &tty_state_log);
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");
    let (output, settings) = run_pty_command(pair, cmd);

    assert_eq!(main_invocation_count(&log), 2, "one retry: {output}");
    assert_eq!(
        std::fs::read_to_string(&tty_state_log)
            .expect("read ssh tty state log")
            .lines()
            .collect::<Vec<_>>(),
        ["sane", "sane"],
        "the guard repairs entry and restores before retry"
    );
    assert_eq!(
        output.matches(rimz::remote::tty::EMULATOR_RESET).count(),
        1,
        "only the dropped session resets the emulator: {output:?}"
    );
    assert_shell_tty(&settings);
}

#[test]
fn recovery_panel_checks_the_configured_http_204_endpoint() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let (probe_url, stop, probe_thread) = http_204_probe();

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "20");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");
    cmd.env("RIMZ_REMOTE_DIAL_MS", "25");
    cmd.env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "500");
    cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "500");
    cmd.env("RIMZ_REMOTE_INTERNET_PROBE", &probe_url);
    let (output, _) = run_pty_command(remote_connect_pty(), cmd);
    stop.store(true, Ordering::Relaxed);
    probe_thread.join().expect("join HTTP probe");

    assert!(
        output.contains("Internet"),
        "internet row is visible: {output}"
    );
    assert!(
        output.contains("127.0.0.1"),
        "the row labels the configured URL host: {output}"
    );
    assert!(
        output.contains("✓  Internet"),
        "HTTP 204 settles the checkpoint as reachable: {output}"
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
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "5000")
        .env("RIMZ_REMOTE_DIAL_MS", "0")
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
    let probe_index = invocations
        .iter()
        .position(|argv| is_probe_invocation(argv))
        .expect("probe stream invocation");
    let master_index = invocations
        .iter()
        .position(|argv| is_master_invocation(argv))
        .expect("master invocation");
    assert!(
        probe_index > master_index,
        "probe stream starts only after the master begins: {invocations:?}"
    );
    assert!(
        invocations[..probe_index]
            .iter()
            .any(|argv| is_control_check_invocation(argv)),
        "readiness checks run before the probe stream: {invocations:?}"
    );
}

#[test]
fn established_link_drop_reconnects_and_notifies_once() {
    let env = Env::new();
    write_link_notify_command_config(&env);
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let notify_log = env.project_root.join("notify.log");
    // First session drops the link (255), the reattach detaches cleanly (0).
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "80")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .env("RIMZ_NOTIFY_TEST_LOG", &notify_log)
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "reconnect ends on the clean detach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log)
        .into_iter()
        .filter(|argv| is_main_invocation(argv))
        .collect::<Vec<_>>();
    assert_eq!(invocations.len(), 2, "dropped once, reattached once");
    let all_invocations = shim_invocations(&log);
    assert_eq!(
        all_invocations
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        2,
        "the initial and recovery masters prove both connections"
    );
    assert!(
        !snippet(&invocations[0]).contains("RIMZ_REMOTE_RECONNECT"),
        "the initial attach stays attended: {:?}",
        invocations[0]
    );
    assert!(
        snippet(&invocations[1]).contains("export RIMZ_REMOTE_RECONNECT=1;"),
        "the retry is marked unattended: {:?}",
        invocations[1]
    );
    assert!(
        invocations[1]
            .iter()
            .any(|arg| arg.starts_with("ControlPath=")),
        "the tty reattach reuses the confirmed master: {:?}",
        invocations[1]
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reconnecting"),
        "the supervisor narrates the retry: {stderr}"
    );
    assert!(
        !stderr.contains("network to dev-box restored — reconnecting now"),
        "a plain link drop must not report network restoration: {stderr}"
    );
    assert!(
        stderr.contains("reattached to dev-box"),
        "plain mode reports the successful handoff: {stderr}"
    );

    let text = wait_for_notify_log(
        &notify_log,
        &[
            "link_lost|RimZ: remote link lost|SSH to dev-box dropped; reconnecting.",
            "link_restored|RimZ: remote link restored|SSH to dev-box is responsive again.",
        ],
    );
    assert_eq!(text.matches("link_lost|").count(), 1, "lost edge: {text}");
    assert_eq!(
        text.matches("link_restored|").count(),
        1,
        "restored edge: {text}"
    );
    assert!(
        text.find("link_lost|") < text.find("link_restored|"),
        "restore follows loss: {text}"
    );
}

#[test]
fn unreachable_endpoint_holds_until_restored_then_reconnects_immediately() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let (ssh_config, address) = closed_ssh_endpoint(&env);
    std::fs::write(&plan, "255\n0\n").expect("write plan");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_DIAL_MS", "25")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 1, Duration::from_secs(2));
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        main_invocation_count(&log),
        1,
        "an unreachable endpoint must not consume reconnect attempts"
    );

    let _listener = TcpListener::bind(address).expect("restore reachable endpoint");
    wait_for_main_invocations(&mut child, &log, 2, Duration::from_secs(3));
    let out = child.wait_with_output().expect("wait for clean reattach");

    assert!(
        out.status.success(),
        "restored endpoint reattaches cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("network to dev-box lost — waiting for network; Ctrl-C stops"),
        "the unreachable hold is visible: {stderr}"
    );
    assert!(
        stderr.contains("network to dev-box restored — reconnecting now"),
        "the network edge is visible: {stderr}"
    );
}

#[test]
fn unreachable_endpoint_retries_at_the_hold_cap() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let (ssh_config, _) = closed_ssh_endpoint(&env);
    std::fs::write(&plan, "255\n0\n").expect("write plan");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_CAP_MS", "200")
        .env("RIMZ_REMOTE_DIAL_MS", "25")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 2, Duration::from_secs(2));
    let out = child.wait_with_output().expect("wait for capped retry");

    assert!(
        out.status.success(),
        "hold-cap retry exits cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        main_invocation_count(&log),
        2,
        "the safety valve launches one retry while dials stay unreachable"
    );
}

#[test]
fn reachable_endpoint_retries_failed_masters_without_stderr_spam() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    let (ssh_config, address) = closed_ssh_endpoint(&env);
    let _listener = TcpListener::bind(address).expect("answer endpoint dials");
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    std::fs::write(&master_plan, "255\n255\n0\n").expect("write master plan");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_CAP_MS", "500")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1")
        .env("RIMZ_REMOTE_DIAL_MS", "20")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 2, Duration::from_secs(2));
    let out = child.wait_with_output().expect("wait for clean reattach");
    assert!(
        out.status.success(),
        "background-master reconnect ends cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        4,
        "two initial failures, one initial success, and one recovery master: {invocations:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("Connection refused").count(),
        1,
        "identical fast failures report only on transition: {stderr}"
    );
}

#[test]
fn reachable_host_and_probe_blackout_kill_a_zombie_transport() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stderr_path = env.project_root.join("stderr.log");
    let ready = env.project_root.join("control-master-ready");
    let ssh_config = env.project_root.join("ssh-config.txt");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind reachable endpoint");
    let address = listener.local_addr().expect("dial address");
    std::fs::write(
        &ssh_config,
        format!("hostname 127.0.0.1\nport {}\n", address.port()),
    )
    .expect("write ssh config fixture");
    let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");

    let started = Instant::now();
    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_PROBE_SILENT_AFTER_ACKS", "2")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "3000")
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .env("RIMZ_REMOTE_BLACKOUT_MS", "100")
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_DIAL_MS", "50")
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 2, Duration::from_secs(2));
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the replacement attach must beat the parked three-second ssh child"
    );
    child
        .kill()
        .expect("stop supervisor after observing reattach");
    child.wait().expect("reap supervisor");
    let stderr = std::fs::read_to_string(&stderr_path).expect("read stderr log");
    assert!(
        stderr.contains(
            "link to dev-box confirmed dead — host reachable, session silent; reconnecting now"
        ),
        "the evidence-backed zombie kill is visible: {stderr}"
    );
}

#[test]
fn remote_web_missing_binary_points_at_setup_after_supervised_master() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let port = reserve_local_port();
    let out = remote_web_command(&env, &log, port)
        .env("RIMZ_TEST_SSH_WEB_PREP_STATUS", "127")
        .bounded_output()
        .expect("run remote web prep without rimz");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rimz is not installed on dev-box"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rimz remote setup dev-box:query-engine"),
        "{stderr}"
    );
    assert_eq!(master_invocation_count(&log), 1, "one supervised master");
    assert_eq!(web_prep_invocation_count(&log), 1, "one preparation");
    assert_eq!(tunnel_invocation_count(&log), 0, "no tunnel opens");
}

#[test]
fn remote_web_emits_prep_url_and_browser_only_after_tunnel_readiness() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stdout_path = env.project_root.join("stdout.log");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    let master_exit_plan = env.project_root.join("master-exit.plan");
    let port = reserve_local_port();
    let (browser_bin, browser_log) = write_browser_shim(&env);
    let ambient_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(browser_bin.clone()).chain(std::env::split_paths(&ambient_path)),
    )
    .expect("browser PATH");
    std::fs::write(&tunnel_plan, "0\n").expect("write tunnel plan");
    std::fs::write(&master_exit_plan, "0\n").expect("write master exit plan");
    let stdout = std::fs::File::create(&stdout_path).expect("create stdout log");

    let mut child = remote_web_command(&env, &log, port)
        .env(
            "RIMZ_TEST_SSH_WEB_PREP_STDERR",
            "remote preparation started\n",
        )
        .env("RIMZ_TEST_SSH_TUNNEL_READY_MS", "300")
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_MS", "900")
        .env("RIMZ_TEST_SSH_MASTER_EXIT_PLAN", &master_exit_plan)
        .env("RIMZ_TEST_BROWSER_LOG", &browser_log)
        .env("PATH", path)
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn remote web connect");

    wait_for_tunnel_invocation(&mut child, &log);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::read_to_string(&stdout_path).unwrap_or_default(),
        "",
        "URL waits for local listener readiness"
    );
    assert!(!browser_log.exists(), "browser waits for tunnel readiness");

    let url = format!("http://127.0.0.1:{port}/rimz-project-a1b2c3");
    let browser = wait_for_notify_log(&browser_log, &[&url]);
    assert_eq!(browser.trim(), url);
    assert!(
        child.try_wait().expect("poll remote web connect").is_none(),
        "browser open must leave the ControlMaster-owned tunnel in the foreground"
    );

    let out = child.wait_with_output().expect("wait remote web connect");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&stdout_path)
            .expect("read stdout")
            .trim(),
        url
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("remote preparation started"),
        "preparation stderr stays visible"
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
}

#[test]
fn remote_web_reconnects_once_after_established_transport_exit() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    let master_exit_plan = env.project_root.join("master-exit.plan");
    let port = reserve_local_port();
    std::fs::write(&tunnel_plan, "0\n0\n").expect("write tunnel plan");
    std::fs::write(&master_exit_plan, "255\n0\n").expect("write master exit plan");

    let out = remote_web_command(&env, &log, port)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_MS", "500")
        .env("RIMZ_TEST_SSH_MASTER_EXIT_PLAN", &master_exit_plan)
        .bounded_output()
        .expect("run reconnecting remote web tunnel");

    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 2);
    assert_eq!(master_invocation_count(&log), 2);
    assert_eq!(web_prep_invocation_count(&log), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("web tunnel to dev-box lost — reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_web_no_reconnect_stays_a_direct_one_shot() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("tunnel.plan");
    let port = reserve_local_port();
    std::fs::write(&plan, "255\n0\n").expect("write tunnel plan");

    let out = remote_web_command(&env, &log, port)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_TUNNEL_LISTEN", "1")
        .env("RIMZ_TEST_SSH_TUNNEL_SLEEP_MS", "80")
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .bounded_output()
        .expect("run one-shot remote web tunnel");

    assert!(!out.status.success());
    assert_eq!(master_invocation_count(&log), 0);
    assert_eq!(web_prep_invocation_count(&log), 1);
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("exited with status 255; not reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_web_fatal_exit_before_readiness_emits_no_url() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("tunnel.plan");
    let port = reserve_local_port();
    std::fs::write(&plan, "2\n").expect("write tunnel plan");

    let out = remote_web_command(&env, &log, port)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .bounded_output()
        .expect("run fatal remote web tunnel");

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "URL must not precede readiness");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("exited with status 2; not reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
}

#[test]
fn remote_web_direct_clean_exit_before_readiness_is_an_error() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("tunnel.plan");
    let port = reserve_local_port();
    std::fs::write(&plan, "0\n").expect("write tunnel plan");

    let out = remote_web_command(&env, &log, port)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .bounded_output()
        .expect("run clean remote web tunnel exit before readiness");

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "URL must not precede readiness");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("web tunnel exited before local port accepted connections"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert_eq!(master_invocation_count(&log), 0);
}

#[test]
fn remote_alias_update_drives_connect_and_reset() {
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
}
