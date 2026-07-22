//! `ssh`-shaped trace shim used by `tests/integration/remote_attach.rs`.
//!
//! Appends one line per invocation to the file at `$RIMZ_TEST_SSH_LOG` of the
//! form `argv0\targv1\t...\n`. When `$RIMZ_TEST_SSH_PLAN` names a file, the
//! shim pops that file's first line and exits with it as its status — the
//! scripted exit sequence the reconnect tests drive (a dropped link is
//! `255`, a clean detach `0`); without a plan it exits 0. Tests reach the
//! shim through the `RIMZ_SSH_BIN` override, never PATH.
//! `$RIMZ_TEST_SSH_RAW_TTY` simulates OpenSSH leaving the controlling tty raw;
//! `$RIMZ_TEST_SSH_TTY_STATE_LOG` records whether each attach inherited sane
//! shell flags before that transition.
//! `$RIMZ_TEST_SSH_MASTER_PLAN` scripts background ControlMaster failures and
//! `$RIMZ_TEST_SSH_MASTER_STDERR` supplies their diagnostic output.
//! `$RIMZ_TEST_SSH_MASTER_READY_PLAN` controls whether each successful master
//! publishes the control socket marker before parking.
//! `$RIMZ_TEST_SSH_MASTER_EXIT_PLAN` and `$RIMZ_TEST_SSH_MASTER_EXIT_MS`
//! script established ControlMaster exits.

use std::env;
use std::fs::OpenOptions;
use std::io::{BufRead, Read, Write};

fn main() {
    let log_path = env::var_os("RIMZ_TEST_SSH_LOG").expect("RIMZ_TEST_SSH_LOG unset");
    let argv = env::args().collect::<Vec<_>>();
    let mut line = argv.join("\t");
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open trace log");
    file.write_all(line.as_bytes()).expect("write trace line");

    if is_config_query(&argv) {
        exit_config_query();
    }

    if is_control_check(&argv) {
        exit_control_check(&argv);
    }

    if is_forward_control(&argv) {
        if env::var_os("RIMZ_TEST_SSH_TUNNEL_PLAN").is_some() {
            run_web_control_forward();
        }
        return;
    }

    if argv
        .iter()
        .any(|arg| arg.contains("remote link-stats ingest"))
    {
        record_probe_before_master_if_needed();
        ack_probe_stream();
        return;
    }

    if argv.iter().any(|arg| arg.contains("rimz web open")) {
        exit_web_prep();
    }

    if argv.iter().any(|arg| arg == "-M") {
        run_control_master(&argv);
    }

    if argv.iter().any(|arg| arg == "-N") {
        run_web_tunnel(&argv);
    }

    publish_control_master_if_requested(&argv);
    wait_for_probe_if_requested(&log_path);
    record_and_enter_raw_tty_if_requested();

    if let Ok(ms) = env::var("RIMZ_TEST_SSH_SLEEP_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    exit_from_plan("RIMZ_TEST_SSH_PLAN");
}

#[cfg(unix)]
fn record_and_enter_raw_tty_if_requested() {
    use nix::sys::termios::{self, InputFlags, LocalFlags, OutputFlags, SetArg};

    let state_log = env::var_os("RIMZ_TEST_SSH_TTY_STATE_LOG");
    let enter_raw = env::var_os("RIMZ_TEST_SSH_RAW_TTY").is_some();
    if state_log.is_none() && !enter_raw {
        return;
    }
    let stdin = std::io::stdin();
    let mut settings = termios::tcgetattr(&stdin).expect("read ssh trace tty state");
    if let Some(path) = state_log {
        let sane = settings.input_flags.contains(InputFlags::ICRNL)
            && settings
                .output_flags
                .contains(OutputFlags::OPOST | OutputFlags::ONLCR)
            && settings
                .local_flags
                .contains(LocalFlags::ICANON | LocalFlags::ISIG | LocalFlags::ECHO);
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open ssh trace tty state log");
        writeln!(log, "{}", if sane { "sane" } else { "damaged" })
            .expect("write ssh trace tty state");
    }
    if enter_raw {
        termios::cfmakeraw(&mut settings);
        termios::tcsetattr(&stdin, SetArg::TCSANOW, &settings).expect("set ssh trace tty raw");
    }
}

fn exit_web_prep() -> ! {
    if let Ok(stderr) = env::var("RIMZ_TEST_SSH_WEB_PREP_STDERR") {
        let mut stream = std::io::stderr().lock();
        stream
            .write_all(stderr.as_bytes())
            .expect("write prep stderr");
        stream.flush().expect("flush prep stderr");
    }
    let output = env::var("RIMZ_TEST_SSH_WEB_PREP_OUTPUT").unwrap_or_else(|_| {
        r#"{"version":"rimz.web.v2","url":"http://127.0.0.1:8200/?arg=rimz-project-a1b2c3","session":"rimz-project-a1b2c3","port":8200,"credential":{"username":"rimz","secret":"test-web-token"}}"#.to_owned()
    });
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .expect("write prep stdout");
    stdout.flush().expect("flush prep stdout");
    let code = env::var("RIMZ_TEST_SSH_WEB_PREP_STATUS")
        .ok()
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    std::process::exit(code);
}

fn run_web_tunnel(argv: &[String]) -> ! {
    if let Ok(ms) = env::var("RIMZ_TEST_SSH_TUNNEL_READY_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    let mut listener = (env::var_os("RIMZ_TEST_SSH_TUNNEL_LISTEN").is_some()
        || env::var_os("RIMZ_TEST_SSH_TUNNEL_HTTP_LOG").is_some())
    .then(|| {
        let forwarding = argv
            .windows(2)
            .find(|args| args[0] == "-L")
            .map(|args| args[1].as_str())
            .expect("tunnel invocation has -L forwarding");
        let local_port = forwarding
            .split(':')
            .nth(1)
            .and_then(|port| port.parse::<u16>().ok())
            .expect("-L forwarding has a local port");
        std::net::TcpListener::bind(("127.0.0.1", local_port)).expect("bind tunnel listener")
    });
    if let Some(log) = env::var_os("RIMZ_TEST_SSH_TUNNEL_HTTP_LOG") {
        let listener = listener.take().expect("HTTP tunnel listener");
        std::thread::spawn(move || serve_web_tunnel(listener, std::path::Path::new(&log)));
    }
    let _listener = listener;
    if let Ok(ms) = env::var("RIMZ_TEST_SSH_TUNNEL_SLEEP_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    exit_from_plan("RIMZ_TEST_SSH_TUNNEL_PLAN");
}

fn serve_web_tunnel(listener: std::net::TcpListener, log: &std::path::Path) {
    for connection in listener.incoming() {
        let Ok(mut stream) = connection else {
            return;
        };
        let Some(head) = read_http_head(&mut stream) else {
            continue;
        };
        let mut output = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .expect("open tunnel HTTP log");
        output.write_all(&head).expect("log tunnel request");
        output.write_all(b"\n---\n").expect("separate requests");

        if head
            .windows(b"Upgrade: websocket".len())
            .any(|window| window.eq_ignore_ascii_case(b"Upgrade: websocket"))
        {
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\nserver-frame")
                .expect("write upgrade response");
            let mut frame = [0_u8; 12];
            stream.read_exact(&mut frame).expect("read client frame");
            output.write_all(&frame).expect("log client frame");
            output.write_all(b"\n---\n").expect("separate client frame");
        } else {
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("write HTTP response");
        }
    }
}

fn read_http_head(stream: &mut impl Read) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while head.len() < 64 * 1024 {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => return None,
            Ok(_) => {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    return Some(head);
                }
            }
        }
    }
    None
}

fn run_web_control_forward() -> ! {
    if let Ok(ms) = env::var("RIMZ_TEST_SSH_TUNNEL_READY_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    exit_from_plan("RIMZ_TEST_SSH_TUNNEL_PLAN");
}

fn run_control_master(argv: &[String]) -> ! {
    if let Ok(stderr) = env::var("RIMZ_TEST_SSH_MASTER_STDERR") {
        let mut stream = std::io::stderr().lock();
        stream
            .write_all(stderr.as_bytes())
            .expect("write master stderr");
        stream.flush().expect("flush master stderr");
    }
    let code = pop_exit_plan("RIMZ_TEST_SSH_MASTER_PLAN").unwrap_or(0);
    if code != 0 {
        std::process::exit(code);
    }
    let publish_ready = pop_exit_plan("RIMZ_TEST_SSH_MASTER_READY_PLAN")
        .map(|value| value != 0)
        .unwrap_or(true);
    if publish_ready {
        publish_control_master_if_requested(argv);
    }
    if let Ok(ms) = env::var("RIMZ_TEST_SSH_MASTER_EXIT_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        exit_from_plan("RIMZ_TEST_SSH_MASTER_EXIT_PLAN");
    }
    loop {
        std::thread::park_timeout(std::time::Duration::from_secs(60));
    }
}

fn exit_from_plan(key: &str) -> ! {
    std::process::exit(pop_exit_plan(key).unwrap_or(0));
}

fn pop_exit_plan(key: &str) -> Option<i32> {
    let plan_path = env::var_os(key)?;
    let plan = std::fs::read_to_string(&plan_path).expect("read exit plan");
    let mut lines = plan.lines();
    let code: i32 = lines
        .next()
        .unwrap_or("0")
        .trim()
        .parse()
        .expect("plan line is an exit code");
    let rest = lines.collect::<Vec<_>>().join("\n");
    std::fs::write(&plan_path, rest).expect("rewrite exit plan");
    Some(code)
}

fn is_config_query(argv: &[String]) -> bool {
    argv.iter().any(|arg| arg == "-G")
}

fn exit_config_query() -> ! {
    let Some(path) = env::var_os("RIMZ_TEST_SSH_G_FILE") else {
        std::process::exit(255);
    };
    let output = std::fs::read(path).expect("read ssh -G fixture");
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&output).expect("write ssh -G fixture");
    stdout.flush().expect("flush ssh -G fixture");
    std::process::exit(0);
}

fn is_control_check(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|args| args[0] == "-O" && args[1] == "check")
}

fn is_forward_control(argv: &[String]) -> bool {
    argv.windows(2)
        .any(|args| args[0] == "-O" && matches!(args[1].as_str(), "forward" | "cancel"))
}

fn exit_control_check(argv: &[String]) -> ! {
    let ready = control_master_marker(argv)
        .map(|path| path.exists())
        .unwrap_or(true);
    std::process::exit(if ready { 0 } else { 255 });
}

fn record_probe_before_master_if_needed() {
    let Some(ready_path) = env::var_os("RIMZ_TEST_CONTROL_MASTER_READY") else {
        return;
    };
    if std::path::PathBuf::from(ready_path).exists() {
        return;
    }
    if let Some(path) = env::var_os("RIMZ_TEST_PROBE_BEFORE_MASTER") {
        std::fs::write(path, b"probe ran before ControlMaster was ready")
            .expect("record fallback probe");
    }
}

fn publish_control_master_if_requested(argv: &[String]) {
    let Some(path) = control_master_marker(argv) else {
        return;
    };
    if let Ok(ms) = env::var("RIMZ_TEST_CONTROL_MASTER_READY_DELAY_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    std::fs::write(path, b"ready").expect("publish control-master marker");
}

/// Use an explicit test marker when one is configured. Otherwise mirror
/// OpenSSH's socket lifecycle at the `ControlPath` itself: successful shim
/// masters publish it, `ssh -O check` observes it, and the supervisor removes
/// it when the owning master guard ends. This keeps a fast failed master from
/// looking established merely because the check process won a scheduler race.
fn control_master_marker(argv: &[String]) -> Option<std::path::PathBuf> {
    if let Some(path) = env::var_os("RIMZ_TEST_CONTROL_MASTER_READY") {
        return Some(path.into());
    }
    argv.windows(2)
        .find(|args| args[0] == "-S")
        .map(|args| std::path::PathBuf::from(&args[1]))
        .or_else(|| {
            argv.iter()
                .find_map(|arg| arg.strip_prefix("ControlPath="))
                .map(std::path::PathBuf::from)
        })
}

fn wait_for_probe_if_requested(log_path: &std::ffi::OsStr) {
    let Some(timeout) = env::var("RIMZ_TEST_WAIT_FOR_PROBE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
    else {
        return;
    };
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if std::fs::read_to_string(log_path)
            .is_ok_and(|log| log.contains("remote link-stats ingest"))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn ack_probe_stream() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let exit_after_acks = env::var("RIMZ_TEST_PROBE_EXIT_AFTER_ACKS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let silent_after_acks = env::var("RIMZ_TEST_PROBE_SILENT_AFTER_ACKS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let mut ack_count = 0u64;
    let reported_port = env::var("RIMZ_TEST_PROBE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok());
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(seq) = value.get("seq").and_then(|seq| seq.as_u64()) else {
            continue;
        };
        if silent_after_acks.is_some_and(|limit| ack_count >= limit) {
            continue;
        }
        match reported_port {
            Some(port) if ack_count > 0 => {
                writeln!(
                    stdout,
                    r#"{{"v":"rimz.link.v1","seq":{seq},"ports":[{port}]}}"#
                )
                .expect("write ack");
            }
            Some(_) => {
                writeln!(stdout, r#"{{"v":"rimz.link.v1","seq":{seq},"ports":[]}}"#)
                    .expect("write ack");
            }
            None => {
                writeln!(stdout, r#"{{"v":"rimz.link.v1","seq":{seq}}}"#).expect("write ack");
            }
        }
        stdout.flush().expect("flush ack");
        ack_count = ack_count.saturating_add(1);
        if exit_after_acks.is_some_and(|limit| ack_count >= limit) {
            remove_control_master_if_requested();
            std::process::exit(0);
        }
    }
}

fn remove_control_master_if_requested() {
    let Some(path) = env::var_os("RIMZ_TEST_REMOVE_CONTROL_MASTER_ON_PROBE_EXIT") else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!("remove control-master marker {}: {err}", path.display()),
    }
}
