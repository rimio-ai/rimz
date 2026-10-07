use std::os::fd::AsFd;
use std::time::Duration;

use nix::pty::{Winsize, openpty};

use super::*;

fn pty(rows: u16, cols: u16) -> nix::pty::OpenptyResult {
    openpty(
        Some(&Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 800,
            ws_ypixel: 600,
        }),
        None,
    )
    .expect("openpty")
}

#[test]
fn sized_tty_is_untouched_without_a_geometry_read() {
    let pty = pty(24, 80);
    let before = rustix::termios::tcgetwinsize(&pty.slave).unwrap();
    assert_eq!(
        repair_with_size(pty.slave.as_fd(), Duration::from_secs(1), |_| panic!(
            "sized tty must not query geometry"
        )),
        WinsizeRepair::Sized
    );
    assert_eq!(rustix::termios::tcgetwinsize(&pty.slave).unwrap(), before);
}

#[test]
fn zero_tty_is_sized_from_content_geometry() {
    for (rows, cols) in [(0, 0), (0, 80), (24, 0)] {
        let pty = pty(rows, cols);
        let result = repair_with_size(pty.slave.as_fd(), Duration::from_secs(1), |_| {
            Ok(Some(PaneContentSize {
                rows: 38,
                cols: 118,
            }))
        });
        assert_eq!(
            result,
            WinsizeRepair::Repaired {
                rows: 38,
                cols: 118
            }
        );
        let size = rustix::termios::tcgetwinsize(&pty.slave).unwrap();
        assert_eq!(
            (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel),
            (38, 118, 0, 0)
        );
    }
}

#[test]
fn absent_or_zero_geometry_times_out_without_writing() {
    for content in [
        None,
        Some(PaneContentSize { rows: 0, cols: 118 }),
        Some(PaneContentSize { rows: 38, cols: 0 }),
    ] {
        let pty = pty(0, 0);
        let before = rustix::termios::tcgetwinsize(&pty.slave).unwrap();
        let mut queries = 0;
        let result = repair_with_size(pty.slave.as_fd(), Duration::from_millis(60), |remaining| {
            assert!(remaining <= Duration::from_millis(60));
            queries += 1;
            Ok(content)
        });
        assert!(
            queries >= 2,
            "geometry must be polled until the budget expires"
        );
        assert!(matches!(result, WinsizeRepair::Unavailable(_)));
        assert_eq!(rustix::termios::tcgetwinsize(&pty.slave).unwrap(), before);
    }
}

#[test]
fn backend_sizing_during_a_geometry_read_is_not_overwritten() {
    let pty = pty(0, 0);
    let result = repair_with_size(pty.slave.as_fd(), Duration::from_secs(1), |_| {
        rustix::termios::tcsetwinsize(
            &pty.master,
            rustix::termios::Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 800,
                ws_ypixel: 600,
            },
        )
        .unwrap();
        Ok(Some(PaneContentSize {
            rows: 38,
            cols: 118,
        }))
    });
    assert_eq!(result, WinsizeRepair::Sized);
    let size = rustix::termios::tcgetwinsize(&pty.slave).unwrap();
    assert_eq!(
        (size.ws_row, size.ws_col, size.ws_xpixel, size.ws_ypixel),
        (24, 80, 800, 600)
    );
}

#[test]
fn read_and_geometry_errors_are_unavailable() {
    let pty = pty(0, 0);
    let result = repair_with_size(pty.slave.as_fd(), Duration::from_secs(1), |_| {
        Err(super::super::MuxErr::NoSessionToAddress)
    });
    assert!(
        matches!(result, WinsizeRepair::Unavailable(ref reason) if reason.contains("session")),
        "{result:?}"
    );
    let file = tempfile::tempfile().unwrap();
    let result = repair_with_size(file.as_fd(), Duration::from_secs(1), |_| {
        panic!("non-tty must not query geometry")
    });
    assert!(
        matches!(result, WinsizeRepair::Unavailable(ref reason) if reason.contains("tty winsize")),
        "{result:?}"
    );
}
