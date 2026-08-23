//! TTY barrier for pacing multi-image kitty graphics output through tmux.

use std::io;
use std::time::{Duration, Instant};

use rimz::sidebar_pane::{BarrierSource, GraphicsReplyScanner, TtyBarrierSource};

const ACK_TIMEOUT: Duration = Duration::from_millis(500);

#[cfg(unix)]
pub(crate) type LiveGraphicsPacer = GraphicsPacer<TtyBarrierSource>;

#[cfg(not(unix))]
pub(crate) type LiveGraphicsPacer = NoopGraphicsPacer;

pub(crate) struct GraphicsPacer<S: BarrierSource> {
    source: S,
    scanner: GraphicsReplyScanner,
    active: bool,
    owed_ack: bool,
    timeout: Duration,
}

impl<S: BarrierSource> GraphicsPacer<S> {
    fn new(source: S) -> Self {
        Self::with_timeout(source, ACK_TIMEOUT)
    }

    fn with_timeout(source: S, timeout: Duration) -> Self {
        Self {
            source,
            scanner: GraphicsReplyScanner::default(),
            active: true,
            owed_ack: false,
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

        self.owed_ack = true;
        if self.read_ack() {
            self.owed_ack = false;
        } else {
            self.disable();
        }
    }

    fn read_ack(&mut self) -> bool {
        self.scanner.reset();
        let deadline = Instant::now() + self.timeout;
        let mut buf = [0_u8; 256];
        loop {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            match self.source.poll_read(&mut buf, remaining) {
                Ok(Some(0)) | Ok(None) => return false,
                Ok(Some(read)) => {
                    if !self.scanner.push(&buf[..read]).is_empty() {
                        return true;
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }

    fn disable(&mut self) {
        self.active = false;
    }

    fn drain_owed_ack(&mut self) {
        if self.owed_ack && self.read_ack() {
            self.owed_ack = false;
        }
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
        self.drain_owed_ack();
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

#[cfg(unix)]
impl GraphicsPacer<TtyBarrierSource> {
    pub(super) fn open() -> Option<Self> {
        TtyBarrierSource::open_raw().ok().map(Self::new)
    }
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
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[test]
    fn pacer_waits_once_per_acknowledged_image() {
        let reads = Rc::new(RefCell::new(ReadCounts::default()));
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new(
                [
                    FakeEvent::Bytes(b"\x1b_Gi=1;OK\x1b\\"),
                    FakeEvent::Bytes(b"\x1b_Gi=2;OK\x1b\\"),
                ],
                Rc::clone(&reads),
            ),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        pacer.wait_for_barrier();

        assert!(pacer.active());
        assert_eq!(reads.borrow().wait_reads, 2);
    }

    #[test]
    fn timeout_disables_pacing_and_stops_future_reads() {
        let reads = Rc::new(RefCell::new(ReadCounts::default()));
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new(
                [FakeEvent::Timeout, FakeEvent::Bytes(b"\x1b_Gi=2;OK\x1b\\")],
                Rc::clone(&reads),
            ),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        pacer.wait_for_barrier();

        assert!(!pacer.active());
        assert_eq!(reads.borrow().wait_reads, 1);
    }

    #[test]
    fn drop_grace_drains_owed_ack_after_timeout() {
        let reads = Rc::new(RefCell::new(ReadCounts::default()));
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new(
                [FakeEvent::Timeout, FakeEvent::Bytes(b"\x1b_Gi=1;OK\x1b\\")],
                Rc::clone(&reads),
            ),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        drop(pacer);

        assert_eq!(reads.borrow().wait_reads, 2);
    }

    #[test]
    fn drop_skips_grace_when_no_ack_owed() {
        let reads = Rc::new(RefCell::new(ReadCounts::default()));
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new([FakeEvent::Bytes(b"\x1b_Gi=1;OK\x1b\\")], Rc::clone(&reads)),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        drop(pacer);

        let reads = reads.borrow();
        assert_eq!(reads.wait_reads, 1);
        assert_eq!(reads.sweep_reads, 1);
    }

    #[test]
    fn drop_grace_stops_at_deadline_when_ack_never_arrives() {
        let reads = Rc::new(RefCell::new(ReadCounts::default()));
        let mut pacer = GraphicsPacer::with_timeout(
            FakeSource::new([FakeEvent::Timeout, FakeEvent::Timeout], Rc::clone(&reads)),
            Duration::from_millis(1),
        );

        pacer.wait_for_barrier();
        drop(pacer);

        let reads = reads.borrow();
        assert_eq!(reads.wait_reads, 2);
        assert_eq!(reads.sweep_reads, 1);
    }

    enum FakeEvent {
        Bytes(&'static [u8]),
        Timeout,
    }

    struct FakeSource {
        events: VecDeque<FakeEvent>,
        reads: Rc<RefCell<ReadCounts>>,
    }

    #[derive(Default)]
    struct ReadCounts {
        wait_reads: usize,
        sweep_reads: usize,
    }

    impl FakeSource {
        fn new(
            events: impl IntoIterator<Item = FakeEvent>,
            reads: Rc<RefCell<ReadCounts>>,
        ) -> Self {
            Self {
                events: events.into_iter().collect(),
                reads,
            }
        }
    }

    impl BarrierSource for FakeSource {
        fn poll_read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
            if timeout.is_zero() {
                self.reads.borrow_mut().sweep_reads += 1;
                return Ok(None);
            }
            self.reads.borrow_mut().wait_reads += 1;
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
