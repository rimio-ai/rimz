//! `ssh`-shaped trace shim used by `tests/integration/remote_attach.rs`.
//!
//! Appends one line per invocation to the file at `$RIMZ_TEST_SSH_LOG` of the
//! form `argv0\targv1\t...\n`. When `$RIMZ_TEST_SSH_PLAN` names a file, the
//! shim pops that file's first line and exits with it as its status — the
//! scripted exit sequence the reconnect tests drive (a dropped link is
//! `255`, a clean detach `0`); without a plan it exits 0. Tests reach the
//! shim through the `RIMZ_SSH_BIN` override, never PATH.

use std::env;
use std::fs::OpenOptions;
use std::io::{BufRead, Write};

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
        exit_control_check();
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

    if argv.iter().any(|arg| arg.contains("web token ensure")) {
        exit_web_token();
    }

    if argv.iter().any(|arg| arg == "-N") {
        run_web_tunnel(&argv);
    }

    publish_control_master_if_requested();

    if let Ok(ms) = env::var("RIMZ_TEST_SSH_SLEEP_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }

    exit_from_plan("RIMZ_TEST_SSH_PLAN");
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
        r#"{"version":"rimz.web.v1","engine":"zellij","url":"http://127.0.0.1:8082/rimz-project-a1b2c3","session":"rimz-project-a1b2c3","base_url":"http://127.0.0.1:8082","ip":"127.0.0.1","port":8082,"token_count":1}"#.to_owned()
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

fn exit_web_token() -> ! {
    let output = env::var("RIMZ_TEST_SSH_WEB_TOKEN_OUTPUT")
        .unwrap_or_else(|_| "test-web-token\n".to_owned());
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(output.as_bytes())
        .expect("write token stdout");
    stdout.flush().expect("flush token stdout");
    let code = env::var("RIMZ_TEST_SSH_WEB_TOKEN_STATUS")
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
    let _listener = env::var_os("RIMZ_TEST_SSH_TUNNEL_LISTEN").map(|_| {
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
    if let Ok(ms) = env::var("RIMZ_TEST_SSH_TUNNEL_SLEEP_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    exit_from_plan("RIMZ_TEST_SSH_TUNNEL_PLAN");
}

fn exit_from_plan(key: &str) -> ! {
    let Some(plan_path) = env::var_os(key) else {
        std::process::exit(0);
    };
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
    std::process::exit(code);
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

fn exit_control_check() -> ! {
    let ready = env::var_os("RIMZ_TEST_CONTROL_MASTER_READY")
        .map(|path| std::path::PathBuf::from(path).exists())
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

fn publish_control_master_if_requested() {
    let Some(path) = env::var_os("RIMZ_TEST_CONTROL_MASTER_READY") else {
        return;
    };
    if let Ok(ms) = env::var("RIMZ_TEST_CONTROL_MASTER_READY_DELAY_MS")
        && let Ok(ms) = ms.parse::<u64>()
    {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
    std::fs::write(path, b"ready").expect("publish control-master marker");
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
        writeln!(stdout, r#"{{"v":"rimz.link.v1","seq":{seq}}}"#).expect("write ack");
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
