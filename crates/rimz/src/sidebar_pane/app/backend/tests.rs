use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};

use nix::pty::{Winsize, openpty};
use ratatui::Terminal;
use ratatui::backend::{Backend, WindowSize};
use ratatui::layout::{Position, Size};

use super::PaneBackend;
use crate::wakeup::heartbeat::SidebarSize;

pub(in crate::sidebar_pane) struct Pty {
    pub(in crate::sidebar_pane) master: std::fs::File,
    pub(in crate::sidebar_pane) slave: Option<OwnedFd>,
}

impl Pty {
    pub(in crate::sidebar_pane) fn open(cols: u16, rows: u16) -> Self {
        let pty = openpty(Some(&winsize(cols, rows)), None).expect("openpty");
        // Raw output: a cooked pty would rewrite `\n` and blur byte asserts.
        let mut termios = nix::sys::termios::tcgetattr(&pty.slave).expect("tcgetattr");
        nix::sys::termios::cfmakeraw(&mut termios);
        nix::sys::termios::tcsetattr(&pty.slave, nix::sys::termios::SetArg::TCSANOW, &termios)
            .expect("tcsetattr");
        nix::fcntl::fcntl(
            &pty.master,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .expect("nonblocking master");
        Self {
            master: pty.master.into(),
            slave: Some(pty.slave),
        }
    }

    pub(in crate::sidebar_pane) fn slave(&self) -> OwnedFd {
        self.slave
            .as_ref()
            .expect("slave open")
            .try_clone()
            .expect("dup slave")
    }

    pub(in crate::sidebar_pane) fn resize(&self, cols: u16, rows: u16) {
        rustix::termios::tcsetwinsize(
            self.master.as_fd(),
            rustix::termios::Winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: cols * 8,
                ws_ypixel: rows * 16,
            },
        )
        .expect("tcsetwinsize");
    }

    /// Everything the pane side has written so far.
    pub(in crate::sidebar_pane) fn drain(&mut self) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match self.master.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        bytes
    }
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: cols * 8,
        ws_ypixel: rows * 16,
    }
}

#[test]
fn geometry_comes_from_the_fd() {
    let pty = Pty::open(40, 12);
    let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();

    assert_eq!(backend.size().unwrap(), Size::new(40, 12));
    let window = backend.window_size().unwrap();
    assert_eq!(window.columns_rows, Size::new(40, 12));
    assert_eq!(window.pixels, Size::new(320, 192));
    assert_eq!(
        backend.pane_size().map(|size| (size.cols, size.rows)),
        Some((40, 12))
    );

    pty.resize(31, 9);
    assert_eq!(backend.size().unwrap(), Size::new(31, 9));
}

#[test]
fn terminal_clear_answers_the_cursor_query_without_a_tty_round_trip() {
    let mut pty = Pty::open(40, 12);
    let mut terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();

    // ratatui's clear asks the backend for the cursor; a DSR query would write
    // `ESC[6n` and block on a reply the detached host can never read.
    terminal.clear().unwrap();
    Write::flush(terminal.backend_mut()).unwrap();

    let written = pty.drain();
    assert!(written.starts_with(b"\x1b[2J"), "{written:?}");
    assert!(
        !written.windows(4).any(|bytes| bytes == b"\x1b[6n"),
        "no cursor report was requested: {written:?}"
    );
    assert_eq!(
        terminal.backend_mut().get_cursor_position().unwrap(),
        Position::ORIGIN
    );
    terminal
        .backend_mut()
        .set_cursor_position(Position::new(3, 4))
        .unwrap();
    assert_eq!(
        terminal.backend_mut().get_cursor_position().unwrap(),
        Position::new(3, 4)
    );
}

#[test]
fn a_zero_geometry_pane_is_unsized_and_draws_nothing_until_resized() {
    let mut pty = Pty::open(0, 0);
    let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();

    assert_eq!(backend.pane_size(), None);
    backend.begin_frame();
    backend.write_all(b"frame").unwrap();
    backend
        .clear_region(ratatui::backend::ClearType::All)
        .unwrap();
    Backend::flush(&mut backend).unwrap();
    backend.end_frame(true).unwrap();
    assert_eq!(pty.drain(), b"");

    pty.resize(39, 20);
    assert_eq!(
        backend.pane_size().map(|size| (size.cols, size.rows)),
        Some((39, 20))
    );
    backend.write_all(b"frame").unwrap();
    Backend::flush(&mut backend).unwrap();
    assert_eq!(pty.drain(), b"frame");
}

#[test]
fn a_frame_holds_raw_and_encoded_bytes_until_ended() {
    for bracket in [false, true] {
        let mut pty = Pty::open(40, 12);
        let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();
        backend.begin_frame();
        backend.write_all(b"frame").unwrap();
        backend.hide_cursor().unwrap();
        Backend::flush(&mut backend).unwrap();
        Write::flush(&mut backend).unwrap();
        assert_eq!(pty.drain(), b"", "flush must not ship a held frame");

        backend.end_frame(bracket).unwrap();
        assert_eq!(
            pty.drain(),
            if bracket {
                b"\x1b[?2026hframe\x1b[?25l\x1b[?2026l".as_slice()
            } else {
                b"frame\x1b[?25l".as_slice()
            }
        );

        backend.write_all(b"outside").unwrap();
        Write::flush(&mut backend).unwrap();
        assert_eq!(
            pty.drain(),
            b"outside",
            "writes outside a frame pass through"
        );
    }
}

#[test]
fn a_write_to_a_closed_pane_is_an_error() {
    let mut pty = Pty::open(40, 12);
    let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();
    pty.slave = None;
    let Pty { master, .. } = pty;
    drop(master);

    backend.write_all(b"frame").unwrap();
    let err = Backend::flush(&mut backend).unwrap_err();
    assert_eq!(err.raw_os_error(), Some(nix::libc::EIO));
}

#[test]
fn the_fixture_sizes_from_its_terminal_wherever_its_output_goes() {
    let output = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/null")
        .unwrap();
    let backend = PaneBackend::ambient(OwnedFd::from(output), || {
        Ok(WindowSize {
            columns_rows: Size::new(40, 12),
            pixels: Size::new(400, 240),
        })
    })
    .unwrap();

    let terminal = Terminal::new(backend).expect("a terminal over output that is no tty");
    assert_eq!(terminal.size().unwrap(), Size::new(40, 12));
    assert_eq!(
        terminal.backend().pane_size(),
        Some(SidebarSize { cols: 40, rows: 12 })
    );
}
