//! Host-local click ordering, a bounded priority slot per subscribed attachment, and the
//! jump's one landing, which the host names at push.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use super::{DiagSink, FocusPush, FocusTraceEvent, PAINT_WAIT, PushPeer, PushVerdict};
use crate::ids::{PaneId, SidebarInstanceId};
use crate::sidebar_pane::app::fetch::Subscribers;
use crate::sidebar_pane::app::input::Wakeup;
use crate::sidebar_pane::app::timing::FOCUS_RESUME_WATCH_WINDOW;

pub(in crate::sidebar_pane::app) struct JumpHost {
    current: Arc<AtomicU64>,
    requests: Mutex<()>,
    state: Mutex<JumpState>,
    /// How long a jump waits from its click for the owners' paint: `PAINT_WAIT` outside tests.
    paint_wait: Duration,
}

impl Default for JumpHost {
    fn default() -> Self {
        Self {
            current: Arc::default(),
            requests: Mutex::default(),
            state: Mutex::default(),
            paint_wait: PAINT_WAIT,
        }
    }
}

#[derive(Default)]
struct JumpState {
    next: u64,
    active: Option<ActiveJump>,
    pushes: HashMap<String, FocusPush>,
    landing: Option<Landing>,
}

/// The one sidebar pane a pushed jump earns images for, until its deadline: the first
/// owner the host resolved at push.
struct Landing {
    pane: PaneId,
    sequence: u64,
    until: Instant,
}

struct ActiveJump {
    stamp: LeaseStamp,
    target: PaneId,
    until: Instant,
    cancel: Option<Weak<std::sync::mpsc::Sender<PushPeer>>>,
    source: SidebarInstanceId,
    nonce: Option<crate::ids::FocusNonce>,
}

#[derive(Clone, Debug)]
pub(super) struct LeaseStamp {
    pub(super) sequence: u64,
    current: Arc<AtomicU64>,
}

impl PartialEq for LeaseStamp {
    fn eq(&self, other: &Self) -> bool {
        self.sequence == other.sequence && Arc::ptr_eq(&self.current, &other.current)
    }
}

impl Eq for LeaseStamp {}

impl LeaseStamp {
    pub(super) fn is_current(&self) -> bool {
        self.current.load(Ordering::SeqCst) == self.sequence
    }
}

pub(super) struct JumpLease {
    host: Arc<JumpHost>,
    subscribers: Subscribers,
    pub(super) stamp: LeaseStamp,
    pub(super) clicked: Instant,
    pub(super) target: PaneId,
}

impl JumpLease {
    pub(super) fn release(&self) {
        self.host.release(&self.stamp, &self.subscribers);
    }
}

impl Drop for JumpLease {
    fn drop(&mut self) {
        self.release();
    }
}

impl JumpState {
    /// A jump superseded before dispatch ends the landing named for it.
    fn revoke_landing(&mut self, sequence: u64) {
        if self
            .landing
            .as_ref()
            .is_some_and(|landing| landing.sequence == sequence)
        {
            self.landing = None;
        }
    }
}

impl JumpHost {
    /// A host whose jumps wait `paint_wait` from the click, for a test that must outlast the
    /// production bound.
    #[cfg(test)]
    pub(in crate::sidebar_pane::app) fn with_paint_wait(paint_wait: Duration) -> Self {
        Self {
            paint_wait,
            ..Self::default()
        }
    }

    /// The instant a jump clicked at `clicked` stops waiting for its owners' paint.
    pub(super) fn paint_until(&self, clicked: Instant) -> Instant {
        clicked + self.paint_wait
    }

    fn lock(&self) -> MutexGuard<'_, JumpState> {
        // State changes replace whole leases and slots, including during unwinding.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn arm(
        self: &Arc<Self>,
        subscribers: Subscribers,
        source: SidebarInstanceId,
        target: PaneId,
        clicked: Instant,
    ) -> Arc<JumpLease> {
        let mut state = self.lock();
        // A host cannot process 2^64 clicks in its lifetime.
        state.next = state
            .next
            .checked_add(1)
            .expect("host click sequence exhausted");
        let stamp = LeaseStamp {
            sequence: state.next,
            current: self.current.clone(),
        };
        self.current.store(stamp.sequence, Ordering::SeqCst);
        if let Some(previous) = state.active.take() {
            state.revoke_landing(previous.stamp.sequence);
            Self::cancel(previous);
        }
        for (_, push) in state.pushes.drain() {
            push.finish(PushVerdict::Superseded, Instant::now());
        }
        state.active = Some(ActiveJump {
            stamp: stamp.clone(),
            target: target.clone(),
            until: self.paint_until(clicked),
            cancel: None,
            source,
            nonce: None,
        });
        drop(state);
        Arc::new(JumpLease {
            host: self.clone(),
            subscribers,
            stamp,
            clicked,
            target,
        })
    }

    fn cancel(active: ActiveJump) {
        if let Some(sender) = active.cancel.and_then(|sender| sender.upgrade()) {
            // This source-only report interrupts recv_timeout without impersonating a peer.
            let _ = sender.send(PushPeer::new(active.source));
        }
    }

    pub(super) fn supersede(&self, subscribers: &Subscribers, diag: &DiagSink) {
        let mut state = self.lock();
        let Some(active) = state.active.take() else {
            return;
        };
        // A host cannot process 2^64 clicks in its lifetime.
        state.next = state
            .next
            .checked_add(1)
            .expect("host click sequence exhausted");
        self.current.store(state.next, Ordering::SeqCst);
        let diag = diag.for_renderer(active.source.clone());
        let event = FocusTraceEvent::JumpProgress {
            sequence: active.stamp.sequence,
            target: active.target.clone(),
            stage: "input_superseded".to_owned(),
            elapsed_ms: super::duration_millis((active.until - self.paint_wait).elapsed()),
        };
        state.revoke_landing(active.stamp.sequence);
        Self::cancel(active);
        for (_, push) in state.pushes.drain() {
            push.finish(PushVerdict::Superseded, Instant::now());
        }
        drop(state);
        self.wake_peers(subscribers);
        diag.trace_focus(event, HashSet::new());
    }

    fn release(&self, stamp: &LeaseStamp, subscribers: &Subscribers) {
        let mut state = self.lock();
        if !stamp.is_current() {
            return;
        }
        self.current.store(0, Ordering::SeqCst);
        if let Some(active) = state.active.take() {
            Self::cancel(active);
        }
        for (_, push) in state.pushes.drain() {
            push.finish(PushVerdict::Superseded, Instant::now());
        }
        drop(state);
        self.wake_peers(subscribers);
    }

    fn wake_peers(&self, subscribers: &Subscribers) {
        for peer in subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .values()
        {
            let _ = peer.wake.try_send(Wakeup::Tick);
        }
    }

    pub(super) fn request<T>(&self, stamp: &LeaseStamp, request: impl FnOnce() -> T) -> Option<T> {
        let _gate = self
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        stamp.is_current().then(request)
    }

    pub(super) fn register_wait(
        &self,
        stamp: &LeaseStamp,
        ack: &super::PushAck,
        nonce: crate::ids::FocusNonce,
    ) {
        let mut state = self.lock();
        if let Some(active) = state
            .active
            .as_mut()
            .filter(|active| active.stamp == *stamp)
        {
            active.cancel = Some(Arc::downgrade(&ack.0));
            active.nonce = Some(nonce);
        }
    }

    pub(super) fn offer(&self, push: FocusPush) {
        let mut state = self.lock();
        if !push.is_current() {
            push.finish(PushVerdict::Superseded, Instant::now());
            return;
        }
        if let Some(previous) = state
            .pushes
            .insert(push.peer.instance_id.as_str().to_owned(), push)
        {
            previous.finish(PushVerdict::Superseded, Instant::now());
        }
    }

    pub(super) fn take(&self, instance: &SidebarInstanceId) -> Option<FocusPush> {
        self.lock().pushes.remove(instance.as_str())
    }

    pub(in crate::sidebar_pane::app) fn forget(&self, instance: &SidebarInstanceId) {
        self.lock().pushes.remove(instance.as_str());
    }

    pub(super) fn accepts(&self, nonce: Option<crate::ids::FocusNonce>, now: Instant) -> bool {
        self.lock()
            .active
            .as_ref()
            .filter(|active| now < active.until)
            .is_none_or(|active| active.nonce.is_some() && active.nonce == nonce)
    }

    /// Name `pane` the jump's landing from its send instant `sent`, while the jump is
    /// current; it replaces an earlier jump's landing.
    pub(super) fn name_landing(&self, stamp: &LeaseStamp, pane: &PaneId, sent: Instant) {
        let mut state = self.lock();
        if !stamp.is_current() {
            return;
        }
        state.landing = Some(Landing {
            pane: pane.clone(),
            sequence: stamp.sequence,
            until: sent + FOCUS_RESUME_WATCH_WINDOW,
        });
    }

    pub(super) fn lands_on(&self, pane: &PaneId, now: Instant) -> bool {
        self.lock()
            .landing
            .as_ref()
            .is_some_and(|landing| landing.pane == *pane && now < landing.until)
    }

    pub(super) fn paint_fence(
        &self,
        source: &SidebarInstanceId,
        nonce: Option<crate::ids::FocusNonce>,
        now: Instant,
    ) -> Option<impl Fn() -> bool + use<>> {
        let state = self.lock();
        if state
            .active
            .as_ref()
            .filter(|active| now < active.until)
            .is_some_and(|active| {
                if active.nonce.is_none() {
                    &active.source != source
                } else {
                    active.nonce != nonce
                }
            })
        {
            return None;
        }
        let stamp = LeaseStamp {
            sequence: self.current.load(Ordering::SeqCst),
            current: self.current.clone(),
        };
        Some(move || stamp.is_current())
    }
}
