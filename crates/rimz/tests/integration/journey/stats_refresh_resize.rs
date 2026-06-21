//! Regression test for `rimz stats --refresh` re-centring after pane resize.
//!
//! Daemon stats panes can be born wide and then narrowed by mux layout work.
//! The refresh loop must redraw at the new PTY width promptly, rather than
//! leaving the first wide frame on screen until the next 60 s data refresh.

#![cfg(unix)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::common::ScrubSessionEnvExt;

const ROWS: u16 = 40;
const WIDE_COLS: u16 = 200;
const NARROW_COLS: u16 = 80;
const TAGLINE: &str = "The control room for your coding agents";
const INITIAL_BUDGET: Duration = Duration::from_secs(5);
const REDRAW_DEADLINE: Duration = Duration::from_secs(5);
const REDRAW_BUDGET: Duration = Duration::from_secs(2);

#[test]
fn stats_refresh_recenters_on_resize() {
    let bin = assert_cmd::cargo::cargo_bin("rimz");
    assert!(bin.exists(), "rimz binary missing: {}", bin.display());

    let xdg = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg tempdir");

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: ROWS,
            cols: WIDE_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&bin);
    cmd.scrub_session_env();
    cmd.args(["stats", "--refresh"]);
    cmd.env("HOME", xdg.path());
    cmd.env("XDG_CONFIG_HOME", xdg.path());
    cmd.env("XDG_STATE_HOME", xdg.path());
    cmd.env("XDG_RUNTIME_DIR", xdg.path());
    cmd.env("XDG_DATA_HOME", xdg.path());
    cmd.env("XDG_CACHE_HOME", xdg.path());
    cmd.env("RIMZ_PRICING_OFFLINE", "1");
    cmd.env_remove("CLAUDE_CONFIG_DIR");
    cmd.env_remove("CODEX_HOME");
    cmd.env_remove("PI_AGENT_DIR");
    cmd.env_remove("PI_CODING_AGENT_SESSION_DIR");
    cmd.env_remove("PI_CODING_AGENT_DIR");
    cmd.env_remove("RUST_LOG");
    let mut child = pair.slave.spawn_command(cmd).expect("spawn rimz stats");
    drop(pair.slave);

    let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, WIDE_COLS, 0)));
    let mut reader = pair.master.try_clone_reader().expect("clone reader");
    let sink = Arc::clone(&parser);
    let reader_thread = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => sink.lock().expect("parser").process(&buf[..n]),
            }
        }
    });

    let initial_col = wait_for_tagline_col(&parser, |_| true, INITIAL_BUDGET);
    let initial_screen = screen(&parser);
    let mut resize_result = Ok(());
    let mut resized_col = None;
    let mut latency = None;
    if initial_col.is_some() {
        let resized_at = Instant::now();
        resize_result = pair.master.resize(PtySize {
            rows: ROWS,
            cols: NARROW_COLS,
            pixel_width: 0,
            pixel_height: 0,
        });
        if resize_result.is_ok() {
            resized_col = wait_for_tagline_col(&parser, |col| col < 30, REDRAW_DEADLINE);
            if resized_col.is_some() {
                latency = Some(resized_at.elapsed());
            }
        }
    }
    let resized_screen = screen(&parser);

    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader_thread.join();

    let initial_col = initial_col
        .unwrap_or_else(|| panic!("stats never rendered the tagline:\n{initial_screen}"));
    assert!(
        initial_col > 60,
        "wide frame should start well right of the edge, got column {initial_col}:\n{initial_screen}",
    );
    resize_result.expect("resize pty");
    let resized_col = resized_col
        .unwrap_or_else(|| panic!("stats did not redraw at the narrow width:\n{resized_screen}"));
    let latency = latency.expect("latency recorded when resized column is observed");
    println!("redrew stats at column {resized_col} in {latency:?} after resize");
    assert!(
        latency < REDRAW_BUDGET,
        "stats took {latency:?} to redraw after resize; it must repaint on \
         resize rather than wait for the 60s refresh",
    );
}

fn wait_for_tagline_col(
    parser: &Arc<Mutex<vt100::Parser>>,
    pred: impl Fn(usize) -> bool,
    budget: Duration,
) -> Option<usize> {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(col) = tagline_col(&screen(parser))
            && pred(col)
        {
            return Some(col);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn screen(parser: &Arc<Mutex<vt100::Parser>>) -> String {
    parser.lock().expect("parser").screen().contents()
}

fn tagline_col(screen: &str) -> Option<usize> {
    screen
        .lines()
        .find_map(|line| line.find(TAGLINE).map(|idx| line[..idx].chars().count()))
}
