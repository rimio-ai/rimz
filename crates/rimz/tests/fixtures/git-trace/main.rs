//! `git`-shaped trace shim for the diff-stats single-flight integration test.
//!
//! Appends one tab-joined `argv` line to `$RIMZ_TEST_GIT_LOG`, then execs the
//! real `git` at `$RIMZ_TEST_REAL_GIT` with the same arguments so the snapshot's
//! per-worktree git probes still return real data. The test prepends the
//! directory holding this binary (linked as `git`) to PATH for the spawned
//! `rimz sidebar snapshot` process, so the sidebar's cached git path lands here
//! and the trace log counts the true cross-process fork rate.
//!
//! Appends are `O_APPEND` writes of one short line, so concurrent shims never
//! interleave — the line count is an honest fork tally across the fleet.

use std::env;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::process::Command;

fn main() {
    let log_path = env::var_os("RIMZ_TEST_GIT_LOG").expect("RIMZ_TEST_GIT_LOG unset");
    let real_git = env::var_os("RIMZ_TEST_REAL_GIT").expect("RIMZ_TEST_REAL_GIT unset");
    let args: Vec<String> = env::args().skip(1).collect();

    let mut line = std::iter::once("git")
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join("\t");
    line.push('\n');
    {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("open git trace log");
        file.write_all(line.as_bytes())
            .expect("write git trace line");
    }

    // exec replaces this process image with the real git, preserving stdio and
    // exit status, so the parent `rimz` sees git's true output. It only returns
    // on failure.
    let err = Command::new(&real_git).args(&args).exec();
    panic!("exec real git ({real_git:?}) failed: {err}");
}
