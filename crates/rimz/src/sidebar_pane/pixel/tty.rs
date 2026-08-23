//! Kitty graphics replies read from the process' controlling terminal.

use std::io;
use std::time::Duration;

const GRAPHICS_REPLY_START: &[u8] = b"\x1b_G";
const ESC: u8 = 0x1b;

pub(super) trait BarrierSource {
    fn poll_read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>>;

    fn restore(&mut self) {}
}

#[derive(Default)]
pub(super) struct GraphicsReplyScanner {
    state: ScannerState,
}

impl GraphicsReplyScanner {
    pub(super) fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut replies = Vec::new();
        for byte in bytes {
            match &mut self.state {
                ScannerState::Seeking { matched } => {
                    if *byte == GRAPHICS_REPLY_START[*matched] {
                        *matched += 1;
                        if *matched == GRAPHICS_REPLY_START.len() {
                            self.state = ScannerState::InReply {
                                payload: Vec::new(),
                                saw_esc: false,
                            };
                        }
                    } else {
                        *matched = usize::from(*byte == GRAPHICS_REPLY_START[0]);
                    }
                }
                ScannerState::InReply { payload, saw_esc } => {
                    if *saw_esc {
                        if *byte == b'\\' {
                            replies.push(std::mem::take(payload));
                            self.state = ScannerState::default();
                            continue;
                        }
                        payload.push(ESC);
                        *saw_esc = false;
                    }
                    if *byte == ESC {
                        *saw_esc = true;
                    } else {
                        payload.push(*byte);
                    }
                }
            }
        }
        replies
    }

    pub(super) fn reset(&mut self) {
        self.state = ScannerState::default();
    }
}

enum ScannerState {
    Seeking { matched: usize },
    InReply { payload: Vec<u8>, saw_esc: bool },
}

impl Default for ScannerState {
    fn default() -> Self {
        Self::Seeking { matched: 0 }
    }
}

#[cfg(unix)]
pub(super) struct TtyBarrierSource {
    tty: std::fs::File,
    saved: Option<nix::sys::termios::Termios>,
}

#[cfg(unix)]
impl TtyBarrierSource {
    pub(super) fn open_raw() -> io::Result<Self> {
        use nix::sys::termios::{self, SetArg};

        let tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")?;
        let saved = termios::tcgetattr(&tty).map_err(nix_to_io)?;
        let mut raw = saved.clone();
        termios::cfmakeraw(&mut raw);
        raw.output_flags = saved.output_flags;
        termios::tcsetattr(&tty, SetArg::TCSANOW, &raw).map_err(nix_to_io)?;
        Ok(Self {
            tty,
            saved: Some(saved),
        })
    }
}

#[cfg(unix)]
impl io::Write for TtyBarrierSource {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tty.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tty.flush()
    }
}

#[cfg(unix)]
impl BarrierSource for TtyBarrierSource {
    fn poll_read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        use std::io::Read;
        use std::os::fd::AsFd;

        use nix::errno::Errno;
        use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

        let mut fds = [PollFd::new(self.tty.as_fd(), PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(timeout).map_err(io::Error::other)?;
        match poll(&mut fds, timeout) {
            Ok(0) => Ok(None),
            Ok(_)
                if fds[0]
                    .revents()
                    .is_some_and(|events| events.contains(PollFlags::POLLIN)) =>
            {
                self.tty.read(buf).map(Some)
            }
            Ok(_) => Ok(None),
            Err(Errno::EINTR) => Err(io::Error::from(io::ErrorKind::Interrupted)),
            Err(err) => Err(nix_to_io(err)),
        }
    }

    fn restore(&mut self) {
        use nix::sys::termios::{self, SetArg};

        if let Some(saved) = self.saved.take() {
            let _ = termios::tcsetattr(&self.tty, SetArg::TCSANOW, &saved);
        }
    }
}

#[cfg(unix)]
impl Drop for TtyBarrierSource {
    fn drop(&mut self) {
        self.restore();
    }
}

#[cfg(unix)]
fn nix_to_io(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}
