//! Regression test for the sidebar's first-frame latency on attach.
//!
//! A Zellij session created in the background has no client, so its sidebar
//! pane's PTY starts at a placeholder size; the renderer's first frame lands in
//! that tiny area. When the user attaches, Zellij resizes the pane (SIGWINCH).
//! The serve loop must redraw at the new size *immediately* — not on its next
//! tick — or the sidebar reads as a multi-second blank pane on attach.
//!
//! This drives the real `rimz-sidebar serve` binary through a PTY: render into
//! a 1x1 pane, resize to a usable size, and assert the full frame appears well
//! before the (deliberately long) tick would fire.

#![cfg(unix)]
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// Tick long enough that a redraw waiting on it is unmistakably "broken": with
/// the bug the frame appears at ~TICK; with the fix it appears right after the
/// resize. The assertion threshold sits comfortably between the two.
const TICK_SECONDS: u64 = 5;
const REDRAW_BUDGET: Duration = Duration::from_secs(2);

/// A throwaway `rimz` whose `sidebar snapshot`/`heartbeat` calls fail fast, so
/// the serve loop renders its degraded placeholder. That frame still carries
/// the degraded banner we scan for — no real ledger needed to prove the loop
/// redrew at the new size.
fn failing_rimz_stub(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("rimz-stub");
    std::fs::write(&path, "#!/bin/sh\nexit 1\n").expect("write stub");
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod");
    path
}

/// The grid we render the renderer's byte stream into — the post-resize size,
/// so we match on what the pane *shows* rather than on raw escape bytes
/// (ratatui interleaves control codes between glyphs).
const GRID_ROWS: u16 = 40;
const GRID_COLS: u16 = 120;

#[test]
fn sidebar_redraws_at_new_size_on_resize() {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_rimz-sidebar"));
    assert!(
        bin.exists(),
        "rimz-sidebar binary missing: {}",
        bin.display()
    );

    // One short XDG root keeps the per-instance wakeup socket path under the
    // 108-byte AF_UNIX limit (workspace id + 35-char instance id + dirs).
    let xdg = tempfile::Builder::new()
        .prefix("rz")
        .rand_bytes(6)
        .tempdir()
        .expect("xdg tempdir");
    let stub = failing_rimz_stub(xdg.path());

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 1,
            cols: 1,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&bin);
    cmd.args([
        "serve",
        "--mux",
        "zellij",
        "--workspace-id",
        "ws_0123456789abcdef01234567",
        "--session-name",
        "rimz-resize-test",
        "--tick-seconds",
        &TICK_SECONDS.to_string(),
    ]);
    cmd.env("RIMZ_BIN", &stub);
    cmd.env("XDG_STATE_HOME", xdg.path());
    cmd.env("XDG_RUNTIME_DIR", xdg.path());
    let mut child = pair.slave.spawn_command(cmd).expect("spawn rimz-sidebar");
    drop(pair.slave);

    // The reader thread feeds one persistent parser, so each grid read is
    // O(grid) rather than re-parsing the whole growing stream per poll, and it
    // never contends with the reader for a separate buffer lock.
    let parser = Arc::new(Mutex::new(vt100::Parser::new(GRID_ROWS, GRID_COLS, 0)));
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

    // Let the first (1x1) frame land and the loop settle into its wait.
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        !parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("Sidebar degraded"),
        "content should not be visible before the pane is given a usable size",
    );

    // Attach: Zellij sizes the pane. Measure how long until the full frame shows.
    let resized_at = Instant::now();
    pair.master
        .resize(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("resize pty");

    let deadline = resized_at + Duration::from_secs(TICK_SECONDS + 3);
    let mut latency = None;
    while Instant::now() < deadline {
        if parser
            .lock()
            .unwrap()
            .screen()
            .contents()
            .contains("Sidebar degraded")
        {
            latency = Some(resized_at.elapsed());
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(pair.master);
    let _ = reader_thread.join();

    let latency = latency.expect("sidebar never rendered content at the resized dimensions");
    println!("redrew at new size {latency:?} after resize (tick = {TICK_SECONDS}s)");
    assert!(
        latency < REDRAW_BUDGET,
        "sidebar took {latency:?} to redraw after resize; it must repaint on \
         resize rather than wait for the {TICK_SECONDS}s tick",
    );
}
