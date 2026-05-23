//! `zellij`-shaped trace shim used by `tests/zellij_backend.rs`.
//!
//! Writes one line per invocation to the file at `$RIMZ_TEST_ZELLIJ_LOG` of
//! the form `argv0\targv1\t...\n`, then exits 0. The test prepends the
//! directory containing this binary (renamed/linked as `zellij`) to PATH
//! before triggering the wakeup walk, so the ledger writer reaches this
//! shim instead of a real Zellij.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;

fn main() {
    let log_path = env::var_os("RIMZ_TEST_ZELLIJ_LOG").expect("RIMZ_TEST_ZELLIJ_LOG unset");
    let line: String = env::args().collect::<Vec<_>>().join("\t");
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .expect("open trace log");
    writeln!(file, "{line}").expect("write trace line");
}
