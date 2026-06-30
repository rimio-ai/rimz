//! `zellij`-shaped trace shim used by `tests/integration/backend/zellij.rs`.
//!
//! Writes one line per invocation to the file at `$RIMZ_TEST_ZELLIJ_LOG` of
//! the form `argv0\targv1\t...\n`, then returns a small zellij-shaped response.
//! Tests set `$RIMZ_TEST_ZELLIJ_MODE` for failure modes that need more than the
//! default trace.

use std::env;
use std::fs::OpenOptions;
use std::io::{self, Write};

fn main() {
    let log_path = env::var_os("RIMZ_TEST_ZELLIJ_LOG").expect("RIMZ_TEST_ZELLIJ_LOG unset");
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

    if cli
        .windows(2)
        .any(|window| window[0] == "action" && window[1] == "list-clients")
    {
        if let Ok(output) = env::var("RIMZ_TEST_ZELLIJ_LIST_CLIENTS") {
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
