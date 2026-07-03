//! `zellij`-shaped trace shim used by `tests/integration/backend/zellij.rs`.
//!
//! Writes one line per invocation to the file at `$RIMZ_TEST_ZELLIJ_LOG` of
//! the form `argv0\targv1\t...\n`, then returns a small zellij-shaped response.
//! Tests set `$RIMZ_TEST_ZELLIJ_MODE` for failure modes that need more than the
//! default trace.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;

fn main() {
    let log_path = env::var_os("RIMZ_TEST_ZELLIJ_LOG").expect("RIMZ_TEST_ZELLIJ_LOG unset");
    let log_path = std::path::PathBuf::from(log_path);
    let args = env::args().collect::<Vec<_>>();
    let line = args.join("\t");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open trace log");
    writeln!(file, "{line}").expect("write trace line");

    let cli = &args[1..];
    if cli.first().is_some_and(|arg| arg == "--version") {
        write_stdout("zellij 0.44.3");
        return;
    }

    if cli.first().is_some_and(|arg| arg == "list-sessions") {
        write_stderr("No active zellij sessions found.");
        std::process::exit(1);
    }

    if cli.first().is_some_and(|arg| arg == "web") {
        match cli.get(1).map(String::as_str) {
            Some("--help") => {
                write_stdout("zellij-web");
                return;
            }
            Some("--status") => {
                let status = web_status_output(&log_path);
                write_stdout_raw(&status);
                return;
            }
            Some("--list-tokens") => {
                if let Ok(tokens) = env::var("RIMZ_TEST_ZELLIJ_WEB_TOKENS") {
                    write_stdout_raw(&tokens);
                }
                return;
            }
            Some("--start")
            | Some("--stop")
            | Some("--create-token")
            | Some("--create-read-only-token")
            | Some("--revoke-token")
            | Some("--revoke-all-tokens") => {
                if cli.get(1).is_some_and(|arg| arg == "--start")
                    && let Ok(stdout) = env::var("RIMZ_TEST_ZELLIJ_WEB_START_STDOUT")
                {
                    write_stdout_raw(&stdout);
                }
                return;
            }
            _ => {}
        }
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "action" && window[1] == "list-clients")
    {
        if let Ok(output) = env::var("RIMZ_TEST_ZELLIJ_LIST_CLIENTS") {
            write_stdout_raw(&output);
        }
        return;
    }

    if cli
        .windows(2)
        .any(|window| window[0] == "action" && window[1] == "list-panes")
    {
        if let Ok(output) = env::var("RIMZ_TEST_ZELLIJ_LIST_PANES") {
            write_stdout_raw(&output);
        }
        return;
    }

    let mode = env::var("RIMZ_TEST_ZELLIJ_MODE").unwrap_or_default();
    if mode == "socket-overflow-on-birth"
        && cli.first().is_some_and(|arg| arg == "attach")
        && cli.get(1).is_some_and(|arg| arg == "--create-background")
    {
        write_stderr("failed to bind socket: File name too long");
        std::process::exit(5);
    }
}

fn web_status_output(log_path: &Path) -> String {
    if command_seen(log_path, "web\t--start")
        && let Ok(after_start) = env::var("RIMZ_TEST_ZELLIJ_WEB_STATUS_AFTER_START")
    {
        return after_start;
    }
    env::var("RIMZ_TEST_ZELLIJ_WEB_STATUS")
        .unwrap_or_else(|_| "Web server is offline, checked: http://127.0.0.1:8082".to_owned())
}

fn command_seen(log_path: &Path, needle: &str) -> bool {
    std::fs::read_to_string(log_path)
        .map(|log| log.lines().any(|line| line.contains(needle)))
        .unwrap_or(false)
}

fn write_stdout(line: &str) {
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "{line}").expect("write zellij trace stdout");
}

fn write_stdout_raw(text: &str) {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{text}").expect("write zellij trace stdout");
}

fn write_stderr(line: &str) {
    let mut stderr = io::stderr().lock();
    writeln!(stderr, "{line}").expect("write zellij trace stderr");
}
