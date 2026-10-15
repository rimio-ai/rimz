//! In-host focus delivery and the bounded destination-paint handshake.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::SidebarInstanceId;
use crate::mux::focus_anchor::FocusAnchor;

use super::fetch::Subscribers;
use super::input::Wakeup;

const PAINT_WAIT: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub(in crate::sidebar_pane) struct PaintedAck(Arc<Sender<()>>);

impl PartialEq for PaintedAck {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PaintedAck {}

impl PaintedAck {
    pub(super) fn new(sender: Sender<()>) -> Self {
        Self(Arc::new(sender))
    }

    pub(in crate::sidebar_pane) fn painted(self) {
        let _ = self.0.send(());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct FocusPush {
    pub(in crate::sidebar_pane) anchor: FocusAnchor,
    pub(in crate::sidebar_pane) ack: PaintedAck,
    pub(super) until: Instant,
}

#[derive(Clone)]
pub(in crate::sidebar_pane) struct FocusPeers {
    subscribers: Subscribers,
    source: SidebarInstanceId,
}

impl FocusPeers {
    pub(super) fn new(subscribers: Subscribers, source: SidebarInstanceId) -> Self {
        Self {
            subscribers,
            source,
        }
    }

    pub(in crate::sidebar_pane) fn dispatch<T>(
        &self,
        anchor: FocusAnchor,
        dispatch: impl FnOnce() -> T,
    ) -> T {
        let until = Instant::now() + PAINT_WAIT;
        let peers: Vec<_> = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
            .filter(|subscriber| subscriber.instance_id != self.source)
            .map(|subscriber| subscriber.wake.clone())
            .collect();
        let (tx, rx) = channel();
        let ack = PaintedAck::new(tx);
        for peer in peers {
            // A full or closed inbox releases its handle with the unsent message.
            let _ = peer.try_send(Wakeup::FocusPush(Box::new(FocusPush {
                anchor: anchor.clone(),
                ack: ack.clone(),
                until,
            })));
        }
        drop(ack);
        if wait_for_paint(until, Instant::now, |timeout| rx.recv_timeout(timeout)) {
            debug!(nonce = %anchor.nonce, target = %anchor.pane_id, "focus push timed out: no destination acknowledged");
        }
        dispatch()
    }
}

fn wait_for_paint(
    until: Instant,
    mut now: impl FnMut() -> Instant,
    mut receive: impl FnMut(Duration) -> Result<(), RecvTimeoutError>,
) -> bool {
    let remaining = until.saturating_duration_since(now());
    remaining.is_zero() || matches!(receive(remaining), Err(RecvTimeoutError::Timeout))
}

#[cfg(test)]
mod tests;
