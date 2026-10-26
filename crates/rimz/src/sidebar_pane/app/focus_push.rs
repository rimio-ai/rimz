//! Click-ordered host jumps, priority push delivery, and post-dispatch verdict tracing.

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

mod lease;
pub(super) use lease::JumpHost;
use lease::{JumpLease, LeaseStamp};

const PAINT_WAIT: Duration = Duration::from_millis(500);
const REPORT_HORIZON: Duration = Duration::from_secs(1);

type PushReceipt = (
    Instant,
    Instant,
    Option<u64>,
    Receiver<PushPeer>,
    Vec<PushPeer>,
);

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
    pub(super) source_view: Option<super::views::ViewId>,
    pub(in crate::sidebar_pane) anchor: FocusAnchor,
    pub(in crate::sidebar_pane) expected_panes:
        Option<Arc<HashMap<PaneId, crate::mux::FittedPane>>>,
    ack: PushAck,
    sent: Instant,
    pub(super) until: Instant,
    pub(super) peer: PushPeer,
    lease: LeaseStamp,
}

impl FocusPush {
    pub(super) fn is_current(&self) -> bool {
        self.lease.is_current()
    }

    fn new(
        anchor: FocusAnchor,
        ack: PushAck,
        instance_id: SidebarInstanceId,
        sent: Instant,
        until: Instant,
        lease: LeaseStamp,
    ) -> Self {
        Self {
            source_view: None,
            anchor,
            expected_panes: None,
            ack,
            sent,
            until,
            peer: PushPeer::new(instance_id),
            lease,
        }
    }

    pub(super) fn elapsed_ms(&self, now: Instant) -> u64 {
        duration_millis(now.duration_since(self.sent))
    }

    pub(in crate::sidebar_pane) fn finish(mut self, verdict: PushVerdict, now: Instant) {
        self.peer.finished_ms = Some(self.elapsed_ms(now));
        self.peer.verdict = Some(verdict);
        let _ = self.ack.0.send(self.peer);
    }

    pub(super) fn finish_painted(mut self, verdict: PushVerdict, painted: Instant, now: Instant) {
        let (elapsed, sign) = if painted >= self.sent {
            (painted.duration_since(self.sent), 1)
        } else {
            (self.sent.duration_since(painted), -1)
        };
        self.peer.painted_ms =
            Some(sign * i64::try_from(duration_millis(elapsed)).unwrap_or(i64::MAX));
        self.finish(verdict, now);
    }
}

pub(super) type FitView =
    Arc<dyn Fn(&FocusAnchor) -> Option<HashMap<PaneId, crate::mux::FittedPane>> + Send + Sync>;

#[derive(Clone)]
pub(in crate::sidebar_pane) struct FocusPeers {
    source_view: Option<super::views::ViewId>,
    subscribers: Subscribers,
    source: SidebarInstanceId,
    fit: Option<FitView>,
    diag: DiagSink,
    jumps: Arc<JumpHost>,
    lease: Option<Arc<JumpLease>>,
}

impl FocusPeers {
    pub(super) fn with_source_view(mut self, view: Option<super::views::ViewId>) -> Self {
        self.source_view = view;
        self
    }
    pub(super) fn trace_stage(&self, stage: &str) {
        let Some(lease) = &self.lease else {
            return;
        };
        self.diag.trace_focus(
            FocusTraceEvent::JumpProgress {
                sequence: lease.stamp.sequence,
                target: lease.target.clone(),
                stage: stage.to_owned(),
                elapsed_ms: duration_millis(lease.clicked.elapsed()),
            },
            HashSet::new(),
        );
    }

    pub(super) fn release(&self) {
        if let Some(lease) = &self.lease {
            lease.release();
        }
    }

    pub(super) fn arm(&self, target: PaneId, clicked: Instant) -> Self {
        let mut peers = self.clone();
        peers.lease = Some(self.jumps.arm(
            self.subscribers.clone(),
            self.source.clone(),
            target,
            clicked,
        ));
        peers.trace_stage("armed");
        peers
    }

    pub(super) fn supersede(&self) {
        self.jumps.supersede(&self.subscribers, &self.diag);
    }

    pub(super) fn request<T>(&self, request: impl FnOnce() -> T) -> Option<T> {
        match &self.lease {
            Some(lease) => {
                self.trace_stage("request_waiting");
                let result = self
                    .jumps
                    .request(&lease.stamp, || {
                        self.trace_stage("request_started");
                        lease.stamp.is_current().then(request)
                    })
                    .flatten();
                if result.is_none() {
                    self.trace_stage("request_superseded");
                }
                result
            }
            None => Some(request()),
        }
    }

    pub(super) fn new(
        subscribers: Subscribers,
        source: SidebarInstanceId,
        diag: DiagSink,
        jumps: Arc<JumpHost>,
    ) -> Self {
        Self {
            source_view: None,
            subscribers,
            source,
            fit: None,
            diag,
            jumps,
            lease: None,
        }
    }

    pub(super) fn clicked(&self) -> Option<Instant> {
        self.lease.as_ref().map(|lease| lease.clicked)
    }

    pub(super) fn is_current(&self) -> bool {
        self.lease
            .as_ref()
            .is_none_or(|lease| lease.stamp.is_current())
    }

    pub(super) fn take_push(&self) -> Option<FocusPush> {
        self.jumps.take(&self.source)
    }

    pub(super) fn accepts(&self, nonce: Option<crate::ids::FocusNonce>, now: Instant) -> bool {
        self.jumps.accepts(nonce, now)
    }

    pub(super) fn paint_permit(
        &self,
        nonce: Option<crate::ids::FocusNonce>,
        now: Instant,
    ) -> Option<impl Fn() -> bool + use<>> {
        self.jumps.paint_permit(&self.source, nonce, now)
    }

    pub(in crate::sidebar_pane) fn with_fit(mut self, fit: Option<FitView>) -> Self {
        self.fit = fit;
        self
    }

    pub(in crate::sidebar_pane) fn dispatch<T>(
        &self,
        anchor: FocusAnchor,
        clicked: Instant,
        dispatch: impl FnOnce() -> T,
    ) -> Option<T> {
        self.dispatch_with(anchor, clicked, dispatch, Instant::now, |rx, timeout| {
            rx.recv_timeout(timeout)
        })
    }

    fn dispatch_with<T>(
        &self,
        anchor: FocusAnchor,
        clicked: Instant,
        dispatch: impl FnOnce() -> T,
        mut now: impl FnMut() -> Instant,
        mut receive: impl FnMut(&Receiver<PushPeer>, Duration) -> Result<PushPeer, RecvTimeoutError>,
    ) -> Option<T> {
        if !self.is_current() {
            self.trace_stage("push_superseded");
            return None;
        }
        let (sent, until, fit_ms, rx, mut traces) = self.push(&anchor, clicked, &mut now)?;
        if !self.is_current() && traces.is_empty() {
            self.trace_stage("push_superseded");
            return None;
        }
        let mut reports = Vec::new();
        let mut outcome = wait_for_paint(
            until,
            &mut now,
            |timeout| {
                if !self.is_current() {
                    return Err(RecvTimeoutError::Disconnected);
                }
                receive(&rx, timeout)
            },
            &mut reports,
        );
        let decided = now();
        let (result, dispatched) = if self.is_current() {
            let result = dispatch();
            let dispatched = now();
            self.release();
            (Some(result), Some(dispatched))
        } else {
            outcome = PushOutcome::Superseded;
            self.release();
            (None, None)
        };
        let horizon = sent + REPORT_HORIZON;
        while let Ok(report) = receive(&rx, horizon.saturating_duration_since(now())) {
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
                budget_ms: duration_millis(until.saturating_duration_since(sent)),
                lead_ms: Some(duration_millis(sent.duration_since(clicked))),
                fit_ms,
                decided_ms: duration_millis(decided.duration_since(sent)),
                dispatched_ms: dispatched
                    .map(|dispatched| duration_millis(dispatched.duration_since(sent))),
                peers: traces,
            },
            HashSet::new(),
        );
        result
    }

    pub(super) fn push(
        &self,
        anchor: &FocusAnchor,
        clicked: Instant,
        mut now: impl FnMut() -> Instant,
    ) -> Option<PushReceipt> {
        let lease = self.lease.as_ref()?;
        let started = now();
        let expected_panes = self.fit.as_ref().and_then(|fit| fit(anchor)).map(Arc::new);
        let peers = self
            .subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (tx, rx) = channel();
        let ack = PushAck::new(tx);
        let mut traces = Vec::with_capacity(peers.len());
        let sent = now();
        let until = clicked + PAINT_WAIT;
        self.jumps.register_wait(&lease.stamp, &ack, anchor.nonce);
        for peer in peers
            .values()
            .filter(|peer| peer.instance_id != self.source)
        {
            if !self.is_current() {
                break;
            }
            let mut push = FocusPush::new(
                anchor.clone(),
                ack.clone(),
                peer.instance_id.clone(),
                sent,
                until,
                lease.stamp.clone(),
            );
            push.expected_panes = expected_panes.clone();
            push.source_view = self.source_view;
            let mut trace = push.peer.clone();
            self.jumps.offer(push);
            trace.delivery = match peer.wake.try_send(Wakeup::Tick) {
                Ok(()) | Err(TrySendError::Full(_)) => PushDelivery::Sent,
                Err(TrySendError::Disconnected(_)) => {
                    self.jumps.forget(&peer.instance_id);
                    PushDelivery::InboxClosed
                }
            };
            traces.push(trace);
        }
        drop(peers);
        drop(ack);
        let fit_ms = self
            .fit
            .as_ref()
            .map(|_| duration_millis(sent.duration_since(started)))
            .filter(|elapsed| *elapsed != 0);
        Some((sent, until, fit_ms, rx, traces))
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
                let painted = matches!(
                    report.verdict,
                    Some(PushVerdict::Painted | PushVerdict::Displaced)
                );
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
