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
    backend.end_frame(true, 0).unwrap();
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

        backend.end_frame(bracket, 40).unwrap();
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
fn a_composed_frame_defers_output_until_explicit_publish() {
    let mut pty = Pty::open(40, 12);
    let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();
    backend.begin_frame();
    backend.write_all(b"composed").unwrap();
    let frame = backend.take_frame(true, 40);
    assert_eq!(pty.drain(), b"");
    backend.begin_frame();
    backend.write_all(b"next").unwrap();
    frame.write(&mut backend).unwrap();
    assert_eq!(pty.drain(), b"\x1b[?2026hcomposed\x1b[?2026l");
    backend.end_frame(false, 40).unwrap();
    assert_eq!(pty.drain(), b"next");
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

fn draw_held_cells(terminal: &mut Terminal<PaneBackend>, columns: u16, rows: u16, graphics: bool) {
    terminal.backend_mut().begin_frame();
    let area = terminal
        .draw(|frame| {
            for y in 0..rows {
                for x in 0..columns {
                    frame.buffer_mut()[(x, y)].set_symbol("x");
                }
            }
        })
        .unwrap()
        .area;
    if graphics {
        terminal
            .backend_mut()
            .write_all(b"\x1b_Ga=d,d=A\x1b\\")
            .unwrap();
    }
    terminal
        .backend_mut()
        .end_frame(graphics, area.width)
        .unwrap();
}

fn sync_pairs(bytes: &[u8]) -> (usize, usize) {
    use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC};
    (
        bytes
            .windows(BEGIN_SYNC.len())
            .filter(|b| *b == BEGIN_SYNC)
            .count(),
        bytes
            .windows(END_SYNC.len())
            .filter(|b| *b == END_SYNC)
            .count(),
    )
}

#[test]
fn cell_frames_bracket_at_half_the_touched_rows_drawn_width() {
    let cases = [
        ("one cell", 1, 1, false),
        ("twenty spinners", 1, 20, false),
        ("counter", 3, 1, false),
        ("below half", 29, 1, false),
        ("exactly half", 30, 1, true),
        ("one whole row", 60, 1, true),
        ("scroll", 55, 18, true),
    ];
    let results = cases.map(|(name, columns, rows, _)| {
        // The fixed viewport, not the backend's 90-column geometry, supplies the width.
        let mut pty = Pty::open(90, 24);
        let mut terminal = Terminal::with_options(
            PaneBackend::for_fd(pty.slave()).unwrap(),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 60, 24)),
            },
        )
        .unwrap();
        draw_held_cells(&mut terminal, columns, rows, false);
        let bytes = pty.drain();
        assert!(!bytes.is_empty(), "{name} must paint");
        (name, sync_pairs(&bytes))
    });
    assert_eq!(
        results,
        cases.map(|(name, _, _, bracket)| (name, (usize::from(bracket), usize::from(bracket))))
    );
}

#[test]
fn graphics_and_dense_cells_share_one_bracket() {
    let results = [1, 60].map(|columns| {
        let mut pty = Pty::open(60, 24);
        let mut terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();
        draw_held_cells(&mut terminal, columns, 1, true);
        sync_pairs(&pty.drain())
    });
    assert_eq!(results, [(1, 1), (1, 1)]);
}

#[test]
fn a_whole_pane_clear_brackets_a_sparse_redraw() {
    let results = [60, 45].map(|width| {
        let mut pty = Pty::open(60, 24);
        let mut terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();
        terminal.backend_mut().begin_frame();
        if width == 60 {
            terminal.clear().unwrap();
        } else {
            pty.resize(width, 24);
        }
        let area = terminal
            .draw(|frame| {
                frame.buffer_mut()[(0, 0)].set_symbol("x");
            })
            .unwrap()
            .area;
        terminal.backend_mut().end_frame(false, area.width).unwrap();
        let bytes = pty.drain();
        assert!(bytes.windows(4).any(|b| b == b"\x1b[2J"), "{bytes:?}");
        sync_pairs(&bytes)
    });
    assert_eq!(results, [(1, 1), (1, 1)]);
}

#[test]
fn a_dense_frame_does_not_bracket_the_next_sparse_frame() {
    let mut pty = Pty::open(60, 24);
    let mut terminal = Terminal::new(PaneBackend::for_fd(pty.slave()).unwrap()).unwrap();
    draw_held_cells(&mut terminal, 60, 1, false);
    let first = sync_pairs(&pty.drain());
    terminal.backend_mut().begin_frame();
    let area = terminal
        .draw(|frame| {
            for x in 0..60 {
                frame.buffer_mut()[(x, 0)].set_symbol(if x == 0 { "y" } else { "x" });
            }
        })
        .unwrap()
        .area;
    terminal.backend_mut().end_frame(false, area.width).unwrap();
    assert_eq!([first, sync_pairs(&pty.drain())], [(1, 1), (0, 0)]);
}

#[test]
fn an_empty_frame_never_brackets() {
    let mut pty = Pty::open(60, 24);
    let mut backend = PaneBackend::for_fd(pty.slave()).unwrap();
    backend.begin_frame();
    backend.end_frame(true, 60).unwrap();
    assert!(pty.drain().is_empty());
}

#[cfg(feature = "testkit")]
#[test]
fn a_capturing_backend_never_brackets() {
    let mut pty = Pty::open(60, 24);
    let mut backend = PaneBackend::captured(60, 24);
    backend.play_output(std::fs::File::from(pty.slave()));
    let mut terminal = Terminal::new(backend).unwrap();
    draw_held_cells(&mut terminal, 60, 24, true);
    assert_eq!(sync_pairs(&pty.drain()), (0, 0));
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
