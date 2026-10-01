//! The one self-skip path. A test that returns early because the host lacks a
//! capability calls [`skip`], which prints the reason and, under a `cargo
//! xtask` run, appends a record so xtask can report the skip instead of
//! counting it as a plain pass.

use std::io::Write;
use std::path::Path;

/// Named by xtask's test sandbox; unset under bare nextest or an editor run.
const SKIP_LOG_ENV: &str = "RIMZ_TEST_SKIP_LOG";

/// The reason every AF_UNIX bind probe skips with.
pub const AF_UNIX_SANDBOXED: &str = "AF_UNIX bind is forbidden in this sandbox";

/// Report that the running test self-skips. `reason` names the missing
/// capability, not the test, so xtask can group skips by it: `tmux not on
/// PATH`, `AF_UNIX bind is forbidden in this sandbox`. The caller returns.
pub fn skip(reason: &str) {
    notice(reason);
    record(
        std::env::var_os(SKIP_LOG_ENV).as_deref().map(Path::new),
        reason,
    );
}

#[expect(
    clippy::print_stderr,
    reason = "a self-skip notice, visible under --no-capture and bare nextest"
)]
fn notice(reason: &str) {
    eprintln!("skipping: {reason}");
}

/// One `<test name>\t<reason>\n` record in a single append, so concurrent
/// test processes never interleave within a line.
fn record(log: Option<&Path>, reason: &str) {
    let Some(log) = log else {
        return;
    };
    let line = format!("{}\t{}\n", test_name(), reason.replace(['\t', '\n'], " "));
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .expect("append self-skip record");
}

/// nextest names the test in its environment; libtest names the thread.
fn test_name() -> String {
    std::env::var("NEXTEST_TEST_NAME")
        .ok()
        .or_else(|| std::thread::current().name().map(str::to_owned))
        .unwrap_or_else(|| "<unknown test>".to_owned())
}

#[test]
fn a_skip_appends_one_record_naming_the_running_test_and_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("skipped-tests");
    record(Some(&log), "tmux not on PATH");
    assert_eq!(
        std::fs::read_to_string(&log).expect("read skip log"),
        "common::skip::a_skip_appends_one_record_naming_the_running_test_and_the_reason\ttmux not on PATH\n"
    );

    record(None, "tmux not on PATH");
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("list dir").count(),
        1,
        "an unset log writes nothing"
    );
}
