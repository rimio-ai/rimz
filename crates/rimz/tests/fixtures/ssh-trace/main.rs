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
use std::io::Write;

fn main() {
    let log_path = env::var_os("RIMZ_TEST_SSH_LOG").expect("RIMZ_TEST_SSH_LOG unset");
    let line: String = env::args().collect::<Vec<_>>().join("\t");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open trace log");
    writeln!(file, "{line}").expect("write trace line");

    let Some(plan_path) = env::var_os("RIMZ_TEST_SSH_PLAN") else {
        return;
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
