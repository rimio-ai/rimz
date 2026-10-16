//! FIFO in-host focus delivery, a bounded paint wait, and post-dispatch verdict tracing.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TrySendError, channel};
use std::time::{Duration, Instant};

use crate::SidebarInstanceId;
use crate::diag::DiagSink;
use crate::diag::focus_trace::{FocusTraceEvent, PushDelivery, PushOutcome, PushPeer, PushVerdict};
use crate::ids::PaneId;
use crate::mux::focus_anchor::FocusAnchor;

use super::fetch::Subscribers;
use super::input::Wakeup;
use super::timing::duration_millis;

const PAINT_WAIT: Duration = Duration::from_millis(100);
const STALE_PAINT_WAIT: Duration = Duration::from_millis(300);
const REPORT_HORIZON: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
pub(in crate::sidebar_pane) struct PushAck(Arc<Sender<PushPeer>>);

impl PartialEq for PushAck {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PushAck {}

impl PushAck {
    pub(super) fn new(sender: Sender<PushPeer>) -> Self {
        Self(Arc::new(sender))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::sidebar_pane) struct FocusPush {
    pub(in crate::sidebar_pane) anchor: FocusAnchor,
    pub(in crate::sidebar_pane) expected_panes:
        Option<Arc<HashMap<PaneId, crate::mux::FittedPane>>>,
    ack: PushAck,
    sent: Instant,
    pub(super) until: Instant,
    pub(super) peer: PushPeer,
}

impl FocusPush {
    pub(super) fn new(
        anchor: FocusAnchor,
        ack: PushAck,
        instance_id: SidebarInstanceId,
        sent: Instant,
        until: Instant,
    ) -> Self {
        Self {
            anchor,
            expected_panes: None,
            ack,
            sent,
            until,
            peer: PushPeer::new(instance_id),
        }
    }

    pub(super) fn elapsed_ms(&self) -> u64 {
        duration_millis(self.sent.elapsed())
    }

    pub(in crate::sidebar_pane) fn finish(mut self, verdict: PushVerdict) {
        self.peer.finished_ms = Some(self.elapsed_ms());
        self.peer.verdict = Some(verdict);
        let _ = self.ack.0.send(self.peer);
    }
}

pub(super) type FitView =
    Arc<dyn Fn(&FocusAnchor) -> Option<HashMap<PaneId, crate::mux::FittedPane>> + Send + Sync>;

#[derive(Clone)]
pub(in crate::sidebar_pane) struct FocusPeers {
    subscribers: Subscribers,
    source: SidebarInstanceId,
    fit: Option<FitView>,
    diag: DiagSink,
}

impl FocusPeers {
    pub(super) fn new(subscribers: Subscribers, source: SidebarInstanceId, diag: DiagSink) -> Self {
        Self {
            subscribers,
            source,
            fit: None,
            diag,
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
        let (sent, until, rx, mut traces) = self.push(&anchor, Instant::now);
        let mut reports = Vec::new();
        let outcome = wait_for_paint(
            until,
            Instant::now,
            |timeout| rx.recv_timeout(timeout),
            &mut reports,
        );
        let decided = Instant::now();
        let result = dispatch();
        let dispatched = Instant::now();
        let horizon = sent + REPORT_HORIZON;
        while let Ok(report) = rx.recv_timeout(horizon.saturating_duration_since(Instant::now())) {
            reports.push(report);
        }
        for mut report in reports {
            if report
                .finished_ms
                .is_some_and(|finished| finished >= duration_millis(REPORT_HORIZON))
            {
                continue;
            }
            if let Some(trace) = traces
                .iter_mut()
                .find(|trace| trace.instance_id == report.instance_id)
            {
                report.delivery = trace.delivery;
                *trace = report;
            }
        }
        self.diag.trace_focus(
            FocusTraceEvent::PushHandshake {
                nonce: anchor.nonce,
                target: anchor.pane_id,
                outcome,
                budget_ms: duration_millis(until.duration_since(sent)),
                decided_ms: duration_millis(decided.duration_since(sent)),
                dispatched_ms: duration_millis(dispatched.duration_since(sent)),
                peers: traces,
            },
            HashSet::new(),
        );
        result
    }

    fn push(
        &self,
        anchor: &FocusAnchor,
        now: impl FnOnce() -> Instant,
    ) -> (Instant, Instant, Receiver<PushPeer>, Vec<PushPeer>) {
        let expected_panes = self.fit.as_ref().and_then(|fit| fit(anchor)).map(Arc::new);
        let budget = if expected_panes.is_some() {
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
            .map(|subscriber| (subscriber.instance_id.clone(), subscriber.wake.clone()))
            .collect();
        let (tx, rx) = channel();
        let ack = PushAck::new(tx);
        let mut traces = Vec::with_capacity(peers.len());
        let sent = now();
        let until = sent + budget;
        for (instance_id, peer) in peers {
            let mut push = FocusPush::new(anchor.clone(), ack.clone(), instance_id, sent, until);
            push.expected_panes = expected_panes.clone();
            let mut trace = push.peer.clone();
            trace.delivery = match peer.try_send(Wakeup::FocusPush(Box::new(push))) {
                Ok(()) => PushDelivery::Sent,
                Err(TrySendError::Full(_)) => PushDelivery::InboxFull,
                Err(TrySendError::Disconnected(_)) => PushDelivery::InboxClosed,
            };
            traces.push(trace);
        }
        drop(ack);
        (sent, until, rx, traces)
    }
}

fn wait_for_paint(
    until: Instant,
    mut now: impl FnMut() -> Instant,
    mut receive: impl FnMut(Duration) -> Result<PushPeer, RecvTimeoutError>,
    reports: &mut Vec<PushPeer>,
) -> PushOutcome {
    loop {
        let remaining = until.saturating_duration_since(now());
        if remaining.is_zero() {
            return PushOutcome::TimedOut;
        }
        let result = receive(remaining);
        let expired = now() >= until;
        match result {
            Ok(report) => {
                let painted = report.verdict == Some(PushVerdict::Painted);
                reports.push(report);
                if expired {
                    return PushOutcome::TimedOut;
                }
                if painted {
                    return PushOutcome::Painted;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return if expired {
                    PushOutcome::TimedOut
                } else {
                    PushOutcome::Released
                };
            }
            Err(RecvTimeoutError::Timeout) => return PushOutcome::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests;
