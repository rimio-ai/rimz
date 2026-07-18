//! Regression test for `rimz stats --refresh` re-centring after pane resize.
//!
//! Daemon stats panes can be born wide and then narrowed by mux layout work.
//! The refresh loop must redraw at the new PTY width promptly, rather than
//! leaving the first wide frame on screen until the next 60 s data refresh.

#![cfg(unix)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

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
    let harness = StatsRefreshHarness::launch(WIDE_COLS);

    let initial_col = wait_for_tagline_col(&harness.parser, |_| true, INITIAL_BUDGET);
    let initial_screen = screen(&harness.parser);
    let mut resize_result = Ok(());
    let mut resized_col = None;
    let mut latency = None;
    if initial_col.is_some() {
        let resized_at = Instant::now();
        resize_result = harness.resize(NARROW_COLS);
        if resize_result.is_ok() {
            resized_col = wait_for_tagline_col(&harness.parser, |col| col < 30, REDRAW_DEADLINE);
            if resized_col.is_some() {
                latency = Some(resized_at.elapsed());
            }
        }
    }
    let resized_screen = screen(&harness.parser);

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

#[test]
fn stats_refresh_drains_input_without_echoing() {
    let mut harness = StatsRefreshHarness::launch(NARROW_COLS);

    let initial_col = wait_for_tagline_col(&harness.parser, |_| true, INITIAL_BUDGET);
    let initial_screen = screen(&harness.parser);
    initial_col.unwrap_or_else(|| panic!("stats never rendered the tagline:\n{initial_screen}"));

    harness
        .write_input(b"fg\x1b[<35;10;10M")
        .expect("write stray input");
    std::thread::sleep(Duration::from_millis(250));
    let after_input = screen(&harness.parser);

    assert!(
        after_input.contains(TAGLINE),
        "stats panel disappeared after stray input:\n{after_input}",
    );
    assert!(
        !after_input.contains("fg"),
        "stats echoed printable input instead of draining it:\n{after_input}",
    );
    assert!(
        !after_input.contains("[<35;10;10M"),
        "stats echoed mouse report instead of draining it:\n{after_input}",
    );
}

#[test]
fn stats_refresh_reloads_on_sigusr1() {
    let mut harness = StatsRefreshHarness::launch(NARROW_COLS);

    let initial_col = wait_for_tagline_col(&harness.parser, |_| true, INITIAL_BUDGET);
    let initial_screen = screen(&harness.parser);
    initial_col.unwrap_or_else(|| panic!("stats never rendered the tagline:\n{initial_screen}"));

    harness.signal_usr1().expect("signal stats reload");
    std::thread::sleep(Duration::from_millis(250));
    let after_signal = screen(&harness.parser);

    assert!(
        harness.is_alive(),
        "stats exited after SIGUSR1 instead of catching the reload signal:\n{after_signal}",
    );
    wait_for_tagline_col(&harness.parser, |_| true, REDRAW_DEADLINE)
        .unwrap_or_else(|| panic!("stats did not render after SIGUSR1 reload:\n{after_signal}"));
}

struct StatsRefreshHarness {
    parser: Arc<Mutex<vt100::Parser>>,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    reader: Option<std::thread::JoinHandle<()>>,
    _xdg: tempfile::TempDir,
}

impl StatsRefreshHarness {
    fn launch(cols: u16) -> Self {
        let bin = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
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
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");

        let mut cmd = CommandBuilder::new(&bin);
        cmd.scrub_session_env();
        cmd.args(["stats", "--refresh"]);
        for key in [
            "HOME",
            "TMPDIR",
            "TMUX_TMPDIR",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "XDG_STATE_HOME",
            "ZELLIJ_CONFIG_DIR",
        ] {
            cmd.env(key, xdg.path());
        }
        cmd.env("RIMZ_PRICING_OFFLINE", "1");
        cmd.env_remove("CLAUDE_CONFIG_DIR");
        cmd.env_remove("CODEX_HOME");
        cmd.env_remove("PI_AGENT_DIR");
        cmd.env_remove("PI_CODING_AGENT_SESSION_DIR");
        cmd.env_remove("PI_CODING_AGENT_DIR");
        cmd.env_remove("RUST_LOG");
        let child = pair.slave.spawn_command(cmd).expect("spawn rimz stats");
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, cols, 0)));
        let writer = pair.master.take_writer().expect("pty writer");
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

        Self {
            parser,
            master: Some(pair.master),
            writer: Some(writer),
            child,
            reader: Some(reader_thread),
            _xdg: xdg,
        }
    }

    fn resize(&self, cols: u16) -> anyhow::Result<()> {
        self.master.as_ref().expect("pty master").resize(PtySize {
            rows: ROWS,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
    }

    fn write_input(&mut self, input: &[u8]) -> std::io::Result<()> {
        let writer = self.writer.as_mut().expect("pty writer");
        writer.write_all(input)?;
        writer.flush()
    }

    fn signal_usr1(&self) -> anyhow::Result<()> {
        let pid = self.child.process_id().expect("stats process id");
        let status = std::process::Command::new("kill")
            .args(["-USR1", &pid.to_string()])
            .status()?;
        anyhow::ensure!(status.success(), "kill -USR1 {pid} exited {status}");
        Ok(())
    }

    fn is_alive(&mut self) -> bool {
        self.child.try_wait().expect("poll stats child").is_none()
    }
}

impl Drop for StatsRefreshHarness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self.writer.take());
        drop(self.master.take());
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
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
