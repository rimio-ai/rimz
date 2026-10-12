//! Best-effort repair of an unsized pane tty before its provider starts.

use std::os::fd::BorrowedFd;
use std::time::{Duration, Instant};

use crate::ids::PaneId;

use super::{MuxBackend, PaneContentSize, Result};

#[derive(Debug, PartialEq, Eq)]
pub enum WinsizeRepair {
    Sized,
    Repaired { rows: u16, cols: u16 },
    Unavailable(String),
}

/// Leave sized ttys untouched; repair zero dimensions from live pane content geometry.
pub fn repair_zero_winsize(
    backend: &dyn MuxBackend,
    pane: &PaneId,
    session: &str,
    tty: BorrowedFd<'_>,
    budget: Duration,
) -> WinsizeRepair {
    repair_with_size(&SystemClock, tty, budget, |remaining| {
        backend.pane_content_size(pane, Some(session), remaining)
    })
}

/// Where the poll loop reads time and waits, so tests can drive it without wall-clock timing.
trait Clock {
    fn now(&self) -> Instant;
    fn sleep(&self, duration: Duration);
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

fn repair_with_size(
    clock: &impl Clock,
    tty: BorrowedFd<'_>,
    budget: Duration,
    mut content_size: impl FnMut(Duration) -> Result<Option<PaneContentSize>>,
) -> WinsizeRepair {
    let deadline = clock.now() + budget;
    loop {
        match rustix::termios::tcgetwinsize(tty) {
            Ok(size) if size.ws_row > 0 && size.ws_col > 0 => return WinsizeRepair::Sized,
            Ok(_) => {}
            Err(error) => {
                return WinsizeRepair::Unavailable(format!("reading tty winsize: {error}"));
            }
        }
        let remaining = deadline.saturating_duration_since(clock.now());
        if remaining.is_zero() {
            return WinsizeRepair::Unavailable(
                "pane content geometry unavailable before deadline".to_owned(),
            );
        }
        let content = match content_size(remaining) {
            Ok(content) => content,
            Err(error) => return WinsizeRepair::Unavailable(error.to_string()),
        };
        if let Some(PaneContentSize { rows, cols }) = content
            && rows > 0
            && cols > 0
        {
            match rustix::termios::tcgetwinsize(tty) {
                Ok(size) if size.ws_row > 0 && size.ws_col > 0 => return WinsizeRepair::Sized,
                Ok(_) => {}
                Err(error) => {
                    return WinsizeRepair::Unavailable(format!("reading tty winsize: {error}"));
                }
            }
            if let Err(error) = rustix::termios::tcsetwinsize(
                tty,
                rustix::termios::Winsize {
                    ws_row: rows,
                    ws_col: cols,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                },
            ) {
                return WinsizeRepair::Unavailable(format!("setting tty winsize: {error}"));
            }
            return WinsizeRepair::Repaired { rows, cols };
        }
        clock.sleep(Duration::from_millis(50).min(deadline.saturating_duration_since(clock.now())));
    }
}

#[cfg(test)]
mod tests;
