//! The data plane the room host runs once for every pane it paints.
//!
//! Each pane subscribes for its own projection of the shared fold.
//! Client views are lent to their painting attachment under one memory-only lock.

use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::sidebar::observe::{self, ObserveMsg};
use crate::sidebar::produce::{HostInputs, PaneProducer};
use crate::utils::time::unix_now_ms;
use crate::wakeup::events::{SidebarEvent, SidebarEventEnvelope};
use crate::{MuxName, RuntimePaths, SidebarInstanceId};

#[cfg(test)]
use super::fetch::FetchUpdate;
use super::fetch::{
    FetchHandle, FetchRequest, HostData, Subscriber, Subscribers, WorkspaceFold, data_tick,
    spawn_fetch_worker,
};
use super::input::Wakeup;
use super::{RoomConfig, ServeConfig, cache_refresh, hidden_views, tmux_watch, transcript_watch};

pub(in crate::sidebar_pane) struct DataPlane {
    pub(super) views: Arc<super::views::ViewsHandle>,
    pub(super) events: Arc<Mutex<crate::sidebar::event_store::EventStore>>,
    pub(super) caps: Arc<Mutex<crate::sidebar_pane::pixel::probe::RoomCaps>>,
    pub(super) geometry: Arc<Mutex<crate::mux::zellij::WidthMemo>>,
    pub(super) tmux_geometry: Arc<Mutex<crate::mux::tmux::WidthMemo>>,
    jumps: Arc<super::focus_push::JumpHost>,
    request_tx: Sender<FetchRequest>,
    pub(super) subscribers: Subscribers,
    host_data: Arc<Mutex<HostData>>,
    observe_tx: SyncSender<ObserveMsg>,
    fit: Option<super::focus_push::FitView>,
    /// The plane's threads, until the first pane starts them.
    lanes: Mutex<Option<Lanes>>,
}

type Lanes = Box<dyn FnOnce() + Send>;

#[derive(Clone)]
pub(in crate::sidebar_pane) struct RoomEvents {
    runtime: RuntimePaths,
    views: Arc<super::views::ViewsHandle>,
    events: Arc<Mutex<crate::sidebar::event_store::EventStore>>,
    subscribers: Subscribers,
    requests: Sender<FetchRequest>,
}

impl RoomEvents {
    pub(super) fn new(
        runtime: &RuntimePaths,
        views: Arc<super::views::ViewsHandle>,
        events: Arc<Mutex<crate::sidebar::event_store::EventStore>>,
        subscribers: Subscribers,
        requests: Sender<FetchRequest>,
    ) -> Self {
        Self {
            runtime: runtime.clone(),
            views,
            events,
            subscribers,
            requests,
        }
    }

    pub(super) fn record(&self, envelope: &SidebarEventEnvelope, received_at_ms: u64) {
        if let SidebarEvent::FocusChanged { focused, unfocused } = &envelope.event {
            self.views
                .lock()
                .note_focus_event(focused, unfocused, envelope.sent_at_ms);
        }
        if envelope.event.is_overlay() {
            lock(&self.events).append(envelope.event.clone(), envelope.sent_at_ms, received_at_ms);
        }
    }

    pub(in crate::sidebar_pane) fn dispatch(&self, envelope: SidebarEventEnvelope) {
        self.record(&envelope, unix_now_ms());
        if let Some(request) = fold_request(&envelope.event) {
            let _ = self.requests.send(request);
        }
        for subscriber in lock(&self.subscribers).values() {
            let _ = subscriber.wake.try_send(Wakeup::Event(envelope.clone()));
        }
        crate::harness::hibernate::wake_focused(&self.runtime, &envelope.event);
    }
}

fn fold_request(event: &SidebarEvent) -> Option<FetchRequest> {
    match event {
        event if event.requests_producer_verification() => Some(FetchRequest::fresh_panes()),
        SidebarEvent::WidthTargetChanged
        | SidebarEvent::BodyFilterChanged
        | SidebarEvent::Notify { .. }
        | SidebarEvent::FocusStranded { .. }
        | SidebarEvent::FocusIntent { .. } => None,
        event if event.is_overlay() => None,
        _ => Some(FetchRequest::default()),
    }
}

impl DataPlane {
    pub(in crate::sidebar_pane) fn room_events(&self, runtime: &RuntimePaths) -> RoomEvents {
        RoomEvents::new(
            runtime,
            self.views.clone(),
            self.events.clone(),
            self.subscribers.clone(),
            self.request_tx.clone(),
        )
    }

    pub(in crate::sidebar_pane) fn focus_peers(
        &self,
        source: SidebarInstanceId,
        diag: crate::diag::DiagSink,
    ) -> super::focus_push::FocusPeers {
        super::focus_push::FocusPeers::new(
            self.subscribers.clone(),
            source,
            diag,
            self.jumps.clone(),
        )
        .with_fit(self.fit.clone())
    }

    /// Threads start with the first pane and live as long as the host. Fetch and production run off every render loop, so animation and input never block on them.
    pub(in crate::sidebar_pane) fn start(
        config: &RoomConfig,
        runtime: &RuntimePaths,
        diag: &crate::diag::DiagSink,
    ) -> Self {
        let (mut plane, request_rx, observe_rx) = Self::idle();
        let observer = diag
            .is_enabled()
            .then(|| observe::RoomObserver::new(plane.observe_tx.clone(), plane.events.clone()));
        let (config, runtime, diag) = (config.clone(), runtime.clone(), diag.clone());
        let subscribers = plane.subscribers.clone();
        let host_data = plane.host_data.clone();
        let requests = plane.request_tx.clone();
        let room_events = plane.room_events(&runtime);
        let hidden = (config.mux == MuxName::Tmux).then(hidden_views::channel);
        if let Some((sweep, _)) = &hidden {
            let sweep = sweep.clone();
            let diag = diag.clone();
            let runtime = runtime.clone();
            plane.fit = Some(Arc::new(move |anchor| {
                sweep.fit_jump(anchor, &diag, &runtime)
            }));
        }
        let lanes: Lanes = Box::new(move || {
            let producer = PaneProducer::seeded(&runtime, &config.session_name);
            let last_projection =
                crate::sidebar::agent_projection::read_publication(&runtime, &config.session_name);
            let inputs =
                HostInputs::seeded(&producer, &config.session_name, last_projection.clone());
            lock(&host_data).inputs = inputs;
            if diag.is_enabled() {
                observe::writer::spawn(runtime.clone(), diag.clone(), observe_rx);
            }
            spawn_fetch_worker(
                config.clone(),
                runtime.clone(),
                diag.clone(),
                producer,
                last_projection,
                request_rx,
                subscribers.clone(),
                host_data.clone(),
                observer,
            );
            cache_refresh::spawn(
                config.clone(),
                runtime.clone(),
                diag.clone(),
                host_data,
                requests,
                move || lock(&subscribers).values().next().map(data_tick),
            );
            // tmux fast path: record control-mode events once and fan out in memory. The plane
            // asks the producer to verify structural changes; the poll stays the presence backstop.
            // Zellij reaches the room socket through its presence plugin.
            if let Some((sweep, rx)) = hidden {
                hidden_views::spawn(rx, config.session_name.clone(), diag, runtime.clone());
                tmux_watch::spawn(
                    runtime.clone(),
                    config.session_name.clone(),
                    sweep,
                    room_events,
                );
            }
            // The host watches every session whose adapter declares transcript-tail context, so mid-turn token and cost updates repaint without waiting for the next hook or tick. Latency only: the tick backstop stays truth.
            transcript_watch::spawn(runtime);
        });
        *lock(&plane.lanes) = Some(lanes);
        plane
    }

    /// The plane's channels with no thread behind them.
    fn idle() -> (Self, Receiver<FetchRequest>, Receiver<ObserveMsg>) {
        let (request_tx, request_rx) = std::sync::mpsc::channel();
        let (observe_tx, observe_rx) = std::sync::mpsc::sync_channel(64);
        let host_data = Arc::new(Mutex::new(HostData::default()));
        let views = lock(&host_data).views.clone();
        let plane = Self {
            views,
            events: Arc::default(),
            caps: Arc::default(),
            geometry: Arc::default(),
            tmux_geometry: Arc::default(),
            jumps: Arc::default(),
            request_tx,
            subscribers: Subscribers::default(),
            host_data,
            observe_tx,
            fit: None,
            lanes: Mutex::new(None),
        };
        (plane, request_rx, observe_rx)
    }

    /// A plane whose requests go nowhere, for tests that drive panes without a fold.
    #[cfg(test)]
    pub(in crate::sidebar_pane) fn detached() -> Self {
        Self::idle().0
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn event_store(&self) -> crate::sidebar::event_store::EventStore {
        lock(&self.events).clone()
    }

    pub(in crate::sidebar_pane) fn hard_refresh(&self) {
        let _ = self.request_tx.send(FetchRequest::hard_refresh());
    }

    /// Feed one more pane. The pane leaves the plane when the subscription drops.
    pub(in crate::sidebar_pane) fn subscribe(
        &self,
        config: &ServeConfig,
        wake: SyncSender<Wakeup>,
    ) -> Subscription {
        let completion = Arc::new(AtomicBool::new(false));
        let (seed, revision) = {
            let views = self.views.lock();
            (views.fold.clone(), views.revision)
        };
        let subscriber = Subscriber {
            instance_id: config.instance_id.clone(),
            own_pane: config.own_pane.clone(),
            tick_seconds: config.tick_seconds,
            completion: completion.clone(),
            wake,
        };
        let membership = Membership {
            instance_id: config.instance_id.clone(),
            subscribers: self.subscribers.clone(),
            jumps: self.jumps.clone(),
        };
        let mut subscribers = lock(&self.subscribers);
        subscribers.insert(config.instance_id.as_str().to_owned(), subscriber);
        drop(subscribers);
        if let Some(start) = lock(&self.lanes).take() {
            // Room workers inherit their creator's Linux comm. Keep their CPU out of the
            // attachment samples by starting them from a separately named thread.
            let mut pending = Some(start);
            std::thread::scope(|scope| {
                let _ = std::thread::Builder::new()
                    .name("rimz-plane".into())
                    .spawn_scoped(scope, || {
                        if let Some(start) = pending.take() {
                            start();
                        }
                    });
            });
            if let Some(start) = pending {
                start();
            }
        }
        Subscription {
            fetch: FetchHandle::new(self.request_tx.clone(), completion),
            observe: self.observe_tx.clone(),
            seed,
            revision,
            _membership: membership,
        }
    }

    #[cfg(test)]
    pub(super) fn remember(&self, fold: WorkspaceFold) {
        let fold = Arc::new(fold);
        self.views.lock().publish(FetchUpdate::Fold {
            fold,
            phase: super::fetch::FetchPhase::Interim,
            source: super::fetch::SnapshotSource::Cached,
        });
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn subscriber_ids(&self) -> Vec<SidebarInstanceId> {
        lock(&self.subscribers)
            .values()
            .map(|subscriber| subscriber.instance_id.clone())
            .collect()
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn take_focus_push(
        &self,
        instance_id: &SidebarInstanceId,
    ) -> Option<super::focus_push::FocusPush> {
        self.focus_peers(instance_id.clone(), crate::diag::DiagSink::disabled())
            .take_push()
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn publish_snapshot(
        &self,
        instance_id: &SidebarInstanceId,
        snapshot: crate::store::snapshot::SidebarSnapshot,
    ) {
        let subscriber = lock(&self.subscribers).get(instance_id.as_str()).cloned();
        if let Some(subscriber) = subscriber {
            {
                let mut views = self.views.lock();
                let frame = views.fold.as_ref().and_then(|fold| fold.frame.clone());
                let fold = match frame {
                    None => super::fixtures::fold(snapshot, Some(&subscriber.own_pane), Vec::new()),
                    Some(frame) => WorkspaceFold {
                        sampled_at: std::time::Instant::now(),
                        workspace: crate::sidebar::enrich::WorkspaceSnapshot(snapshot),
                        frame: Some(frame),
                    },
                };
                views.publish(FetchUpdate::Fold {
                    fold: Arc::new(fold),
                    phase: super::fetch::FetchPhase::Final,
                    source: super::fetch::SnapshotSource::Produced,
                });
                subscriber.completion.store(true, Ordering::Relaxed);
            }
            let _ = subscriber.wake.try_send(Wakeup::Snapshot);
        }
    }

    #[cfg(test)]
    pub(super) fn publish_fold(&self, fold: WorkspaceFold) {
        let cycle: Vec<_> = lock(&self.subscribers).values().cloned().collect();
        {
            let mut views = self.views.lock();
            views.publish(FetchUpdate::Fold {
                fold: Arc::new(fold),
                phase: super::fetch::FetchPhase::Final,
                source: super::fetch::SnapshotSource::Produced,
            });
            for subscriber in &cycle {
                subscriber.completion.store(true, Ordering::Relaxed);
            }
        }
        for subscriber in cycle {
            let _ = subscriber.wake.try_send(Wakeup::Snapshot);
        }
    }
}

/// One pane's ends of the plane.
pub(in crate::sidebar_pane) struct Subscription {
    pub(super) fetch: FetchHandle,
    pub(super) revision: u64,
    pub(super) observe: SyncSender<ObserveMsg>,
    pub(super) seed: Option<Arc<WorkspaceFold>>,
    _membership: Membership,
}

struct Membership {
    instance_id: SidebarInstanceId,
    subscribers: Subscribers,
    jumps: Arc<super::focus_push::JumpHost>,
}

impl Drop for Membership {
    fn drop(&mut self) {
        let mut subscribers = lock(&self.subscribers);
        subscribers.remove(self.instance_id.as_str());
        self.jumps.forget(&self.instance_id);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Room state is memory-only; a poisoned peer does not invalidate the last committed values.
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests;
