//! Terminal backend for one sidebar pane, bound to an owned output fd rather than the process terminal, so the room host can paint a pane whose tty it does not hold.
//!
//! Drawing goes through crossterm's encoder; geometry is read from the fd, and the cursor query is answered from the last position set, because crossterm's own answers round-trip the controlling tty a detached host lacks. A pane the mux has not laid out yet reports `0x0`: such a pane is unsized, and everything written to it is dropped until a read finds it sized. Output that is no terminal at all (a pipe, a file) has no geometry to wait for and is written as it comes.

use std::cell::Cell;
use std::io::{self, BufWriter, Write};
use std::os::fd::OwnedFd;

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell as BufferCell;
use ratatui::layout::{Position, Size};

use crate::sidebar_pane::pixel::{BEGIN_SYNC, END_SYNC};
use crate::wakeup::heartbeat::SidebarSize;

pub(in crate::sidebar_pane) struct PaneBackend {
    output: Box<dyn Write + Send>,
    frame: Option<Vec<u8>>,
    geometry: Geometry,
    sized: Cell<bool>,
    cursor: Position,
}

enum Geometry {
    Fd(OwnedFd),
    /// The fallback worker's own terminal, which its output need not be.
    Ambient(fn() -> io::Result<WindowSize>),
    #[cfg(test)]
    Absent,
}

impl PaneBackend {
    pub(super) fn begin_frame(&mut self) {
        self.frame = Some(Vec::new());
    }

    pub(super) fn end_frame(&mut self, bracket: bool) -> io::Result<()> {
        let frame = self.frame.take().unwrap_or_default();
        let bracket = bracket && !frame.is_empty();
        if bracket {
            self.output.write_all(BEGIN_SYNC)?;
        }
        let body_result = self.output.write_all(&frame);
        let end_result = if bracket {
            self.output.write_all(END_SYNC)
        } else {
            Ok(())
        };
        let flush_result = self.output.flush();
        body_result.and(end_result).and(flush_result)
    }

    /// Paint the pane behind `fd`, the pane's own terminal output.
    pub(in crate::sidebar_pane) fn for_fd(fd: OwnedFd) -> io::Result<Self> {
        let writer = std::fs::File::from(fd.try_clone()?);
        Ok(Self::painting(writer, Geometry::Fd(fd)))
    }

    /// Paint into `output`, reading geometry through `read`: the fallback
    /// worker's own terminal, wherever its output goes.
    pub(in crate::sidebar_pane) fn ambient(
        output: OwnedFd,
        read: fn() -> io::Result<WindowSize>,
    ) -> io::Result<Self> {
        Ok(Self::painting(
            std::fs::File::from(output),
            Geometry::Ambient(read),
        ))
    }

    fn painting(output: std::fs::File, geometry: Geometry) -> Self {
        let backend = Self {
            output: Box::new(BufWriter::new(output)),
            frame: None,
            geometry,
            sized: Cell::new(true),
            cursor: Position::ORIGIN,
        };
        backend.pane_size();
        backend
    }

    /// A backend with no terminal behind it: geometry reads fail, as they do for a process whose output is a pipe.
    #[cfg(test)]
    pub(in crate::sidebar_pane) fn headless(writer: impl Write + Send + 'static) -> Self {
        Self {
            output: Box::new(writer),
            frame: None,
            geometry: Geometry::Absent,
            sized: Cell::new(true),
            cursor: Position::ORIGIN,
        }
    }

    /// The pane's cell size, or `None` while the mux has not sized it.
    pub(in crate::sidebar_pane) fn pane_size(&self) -> Option<SidebarSize> {
        let size = self.read_window().ok()?.columns_rows;
        self.sized.get().then_some(SidebarSize {
            cols: size.width,
            rows: size.height,
        })
    }

    /// Whether the last geometry read found a terminal the mux has not laid out.
    pub(in crate::sidebar_pane) fn is_unsized(&self) -> bool {
        !self.sized.get()
    }

    /// Cell height/width from the pane's pixel geometry, when the terminal reports one.
    pub(in crate::sidebar_pane) fn cell_aspect(&self) -> Option<crate::config::CellAspect> {
        let window = self.read_window().ok()?;
        crate::sidebar_pane::pets::cell_aspect(
            (window.columns_rows.width, window.columns_rows.height),
            (window.pixels.width, window.pixels.height),
        )
    }

    fn read_window(&self) -> io::Result<WindowSize> {
        let window = match &self.geometry {
            Geometry::Fd(fd) => {
                let size = rustix::termios::tcgetwinsize(fd)?;
                WindowSize {
                    columns_rows: Size::new(size.ws_col, size.ws_row),
                    pixels: Size::new(size.ws_xpixel, size.ws_ypixel),
                }
            }
            Geometry::Ambient(read) => read()?,
            #[cfg(test)]
            Geometry::Absent => return Err(io::Error::other("no terminal behind this backend")),
        };
        let cells = window.columns_rows;
        self.sized.set(cells.width != 0 && cells.height != 0);
        Ok(window)
    }
}

impl Write for PaneBackend {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.sized.get() {
            return Ok(buf.len());
        }
        if let Some(frame) = &mut self.frame {
            return frame.write(buf);
        }
        self.output.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.frame.is_some() {
            return Ok(());
        }
        self.output.flush()
    }
}

impl Backend for PaneBackend {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a BufferCell)>,
    {
        if !self.sized.get() {
            return Ok(());
        }
        CrosstermBackend::new(self).draw(content)
    }

    fn append_lines(&mut self, n: u16) -> io::Result<()> {
        if !self.sized.get() {
            return Ok(());
        }
        CrosstermBackend::new(self).append_lines(n)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        if !self.sized.get() {
            return Ok(());
        }
        CrosstermBackend::new(self).hide_cursor()
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        if !self.sized.get() {
            return Ok(());
        }
        CrosstermBackend::new(self).show_cursor()
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        Ok(self.cursor)
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.cursor = position.into();
        if !self.sized.get() {
            return Ok(());
        }
        let cursor = self.cursor;
        CrosstermBackend::new(self).set_cursor_position(cursor)
    }

    fn clear(&mut self) -> io::Result<()> {
        self.clear_region(ClearType::All)
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        if !self.sized.get() {
            return Ok(());
        }
        CrosstermBackend::new(self).clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        Ok(self.read_window()?.columns_rows)
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.read_window()
    }

    fn flush(&mut self) -> io::Result<()> {
        Write::flush(self)
    }
}

#[cfg(test)]
pub(in crate::sidebar_pane) mod tests;
