//! TTY barrier for pacing multi-image kitty graphics output through tmux.

use std::io;
use std::time::{Duration, Instant};

const ACK_TIMEOUT: Duration = Duration::from_millis(250);
const GRAPHICS_REPLY_START: &[u8] = b"\x1b_G";
const ESC: u8 = 0x1b;

#[cfg(unix)]
pub(crate) type LiveGraphicsPacer = GraphicsPacer<TtyBarrierSource>;

#[cfg(not(unix))]
pub(crate) type LiveGraphicsPacer = NoopGraphicsPacer;

pub(crate) struct GraphicsPacer<S: BarrierSource> {
    source: S,
    scanner: GraphicsAckScanner,
    active: bool,
    timeout: Duration,
}

impl<S: BarrierSource> GraphicsPacer<S> {
    fn new(source: S) -> Self {
        Self::with_timeout(source, ACK_TIMEOUT)
    }

    fn with_timeout(source: S, timeout: Duration) -> Self {
        Self {
            source,
            scanner: GraphicsAckScanner::default(),
            active: true,
            timeout,
        }
    }

    pub(super) fn active(&self) -> bool {
        self.active
    }

    pub(super) fn wait_for_barrier(&mut self) {
        if !self.active {
            return;
        }

        self.scanner.reset();
        let deadline = Instant::now() + self.timeout;
        let mut buf = [0_u8; 256];
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                self.disable();
                return;
            };
            match self.source.poll_read(&mut buf, remaining) {
                Ok(Some(0)) | Ok(None) => {
                    self.disable();
                    return;
                }
                Ok(Some(read)) => {
                    if self.scanner.push(&buf[..read]) {
                        return;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    self.disable();
                    return;
                }
            }
        }
    }

    fn disable(&mut self) {
        self.active = false;
    }

    fn drain(&mut self) {
        let mut buf = [0_u8; 256];
        loop {
            match self.source.poll_read(&mut buf, Duration::ZERO) {
                Ok(Some(0)) | Ok(None) | Err(_) => return,
                Ok(Some(_)) => {}
            }
        }
    }
}

impl<S: BarrierSource> Drop for GraphicsPacer<S> {
    fn drop(&mut self) {
        self.drain();
        self.source.restore();
    }
}

pub(crate) trait PixelPacer {
    fn active(&self) -> bool;
    fn wait_for_barrier(&mut self);
}

impl<S: BarrierSource> PixelPacer for GraphicsPacer<S> {
    fn active(&self) -> bool {
        Self::active(self)
    }

    fn wait_for_barrier(&mut self) {
        Self::wait_for_barrier(self);
    }
}

pub(crate) trait BarrierSource {
    fn poll_read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>>;

    fn restore(&mut self) {}
}

#[derive(Default)]
struct GraphicsAckScanner {
    state: ScannerState,
}

impl GraphicsAckScanner {
    fn push(&mut self, bytes: &[u8]) -> bool {
        for byte in bytes {
            match &mut self.state {
                ScannerState::Seeking { matched } => {
                    if *byte == GRAPHICS_REPLY_START[*matched] {
                        *matched += 1;
                        if *matched == GRAPHICS_REPLY_START.len() {
                            self.state = ScannerState::InReply { saw_esc: false };
                        }
                    } else {
                        *matched = usize::from(*byte == GRAPHICS_REPLY_START[0]);
                    }
                }
                ScannerState::InReply { saw_esc } => {
                    if *saw_esc && *byte == b'\\' {
                        self.reset();
                        return true;
                    }
                    *saw_esc = *byte == ESC;
                }
            }
        }
        false
    }

    fn reset(&mut self) {
        self.state = ScannerState::default();
    }
}

enum ScannerState {
    Seeking { matched: usize },
    InReply { saw_esc: bool },
}

impl Default for ScannerState {
    fn default() -> Self {
        Self::Seeking { matched: 0 }
    }
}

#[cfg(unix)]
pub(crate) struct TtyBarrierSource {
    tty: std::fs::File,
    saved: Option<nix::sys::termios::Termios>,
}

#[cfg(unix)]
impl GraphicsPacer<TtyBarrierSource> {
    pub(super) fn open() -> Option<Self> {
        TtyBarrierSource::open().ok().map(Self::new)
    }
}

#[cfg(unix)]
impl TtyBarrierSource {
    fn open() -> io::Result<Self> {
        use nix::sys::termios::{self, SetArg};

        let tty = std::fs::OpenOptions::new().read(true).open("/dev/tty")?;
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
fn nix_to_io(err: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(err as i32)
}

#[cfg(not(unix))]
pub(crate) struct NoopGraphicsPacer;

#[cfg(not(unix))]
impl NoopGraphicsPacer {
    pub(super) fn open() -> Option<Self> {
        None
    }
}

#[cfg(not(unix))]
impl PixelPacer for NoopGraphicsPacer {
    fn active(&self) -> bool {
        false
    }

    fn wait_for_barrier(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn scanner_accepts_partial_graphics_reply() {
        let mut scanner = GraphicsAckScanner::default();

        assert!(!scanner.push(b"\x1b"));
        assert!(!scanner.push(b"_G;OK"));
        assert!(scanner.push(b"\x1b\\"));
    }

    #[test]
    fn scanner_ignores_unrelated_bytes() {
        let mut scanner = GraphicsAckScanner::default();

        assert!(!scanner.push(b"noise\x1b]0;title\x1b\\"));
        assert!(scanner.push(b"\x1b_Gi=42;OK\x1b\\"));
    }

    #[test]
    fn pacer_waits_once_per_acknowledged_image() {
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new([
                FakeEvent::Bytes(b"\x1b_Gi=1;OK\x1b\\"),
                FakeEvent::Bytes(b"\x1b_Gi=2;OK\x1b\\"),
            ]),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        pacer.wait_for_barrier();

        assert!(pacer.active());
        assert_eq!(pacer.source.wait_reads, 2);
    }

    #[test]
    fn timeout_disables_pacing_and_stops_future_reads() {
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new([FakeEvent::Timeout, FakeEvent::Bytes(b"\x1b_Gi=2;OK\x1b\\")]),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        pacer.wait_for_barrier();

        assert!(!pacer.active());
        assert_eq!(pacer.source.wait_reads, 1);
    }

    enum FakeEvent {
        Bytes(&'static [u8]),
        Timeout,
    }

    struct FakeSource {
        events: VecDeque<FakeEvent>,
        wait_reads: usize,
    }

    impl FakeSource {
        fn new(events: impl IntoIterator<Item = FakeEvent>) -> Self {
            Self {
                events: events.into_iter().collect(),
                wait_reads: 0,
            }
        }
    }

    impl BarrierSource for FakeSource {
        fn poll_read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
            if timeout.is_zero() {
                return Ok(None);
            }
            self.wait_reads += 1;
            match self.events.pop_front() {
                Some(FakeEvent::Bytes(bytes)) => {
                    let len = bytes.len().min(buf.len());
                    buf[..len].copy_from_slice(&bytes[..len]);
                    Ok(Some(len))
                }
                Some(FakeEvent::Timeout) | None => Ok(None),
            }
        }
    }
}
