//! In-host focus delivery and the bounded destination-paint handshake.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};
use tracing::debug;

use crate::SidebarInstanceId;
use crate::ids::PaneId;
use crate::mux::focus_anchor::FocusAnchor;

use super::fetch::Subscribers;
use super::input::Wakeup;

const PAINT_WAIT: Duration = Duration::from_millis(100);
const STALE_PAINT_WAIT: Duration = Duration::from_millis(300);

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
    pub(in crate::sidebar_pane) expected_panes:
        Option<Arc<HashMap<PaneId, crate::mux::FittedPane>>>,
    pub(super) until: Instant,
}

pub(super) type FitView =
    Arc<dyn Fn(&FocusAnchor) -> Option<HashMap<PaneId, crate::mux::FittedPane>> + Send + Sync>;

#[derive(Clone)]
pub(in crate::sidebar_pane) struct FocusPeers {
    subscribers: Subscribers,
    source: SidebarInstanceId,
    fit: Option<FitView>,
}

impl FocusPeers {
    pub(super) fn new(subscribers: Subscribers, source: SidebarInstanceId) -> Self {
        Self {
            subscribers,
            source,
            fit: None,
        }
    }

    pub(in crate::sidebar_pane) fn with_fit(mut self, fit: Option<FitView>) -> Self {
        self.fit = fit;
        self
    }

    pub(in crate::sidebar_pane) fn dispatch<T>(
        &self,
        anchor: FocusAnchor,
        dispatch: impl FnOnce() -> T,
    ) -> T {
        let started = Instant::now();
        let (until, rx) = self.push(&anchor, Instant::now);
        let timed_out = wait_for_paint(until, Instant::now, |timeout| rx.recv_timeout(timeout));
        debug!(nonce = %anchor.nonce, target = %anchor.pane_id, elapsed_ms = started.elapsed().as_millis(), timed_out, "focus push completed");
        if timed_out {
            debug!(nonce = %anchor.nonce, target = %anchor.pane_id, "focus push timed out: no destination acknowledged");
        }
        dispatch()
    }

    fn push(&self, anchor: &FocusAnchor, now: impl FnOnce() -> Instant) -> (Instant, Receiver<()>) {
        let expected_panes = self.fit.as_ref().and_then(|fit| fit(anchor)).map(Arc::new);
        let until = now()
            + if expected_panes.is_some() {
                STALE_PAINT_WAIT
            } else {
                PAINT_WAIT
            };
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
                expected_panes: expected_panes.clone(),
                until,
            })));
        }
        drop(ack);
        (until, rx)
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
