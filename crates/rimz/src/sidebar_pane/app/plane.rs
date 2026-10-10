//! The data plane the room host runs once for every pane it paints.
//!
//! Each pane subscribes for its own projection of the shared fold.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::sidebar::observe::{self, ObserveMsg};
use crate::{MuxName, RuntimePaths, SidebarInstanceId};

#[cfg(test)]
use super::fetch::FetchUpdate;
use super::fetch::{
    Covered, FetchRequest, ResultReceiver, Stand, Subscriber, Subscribers, result_channel,
    spawn_fetch_worker,
};
use super::{ServeConfig, cache_refresh, tmux_watch, transcript_watch};

pub(in crate::sidebar_pane) struct DataPlane {
    pub(super) observe_events: Option<Arc<Mutex<crate::sidebar::event_store::EventStore>>>,
    pub(super) caps: Arc<Mutex<crate::sidebar_pane::pixel::probe::RoomCaps>>,
    pub(super) geometry: Arc<Mutex<crate::mux::zellij::WidthMemo>>,
    request_tx: Sender<FetchRequest>,
    subscribers: Subscribers,
    covered: Covered,
    observe_tx: SyncSender<ObserveMsg>,
    /// The plane's threads, until the first pane starts them.
    lanes: Mutex<Option<Lanes>>,
}

type Lanes = Box<dyn FnOnce() + Send>;

impl DataPlane {
    /// Threads start with the first pane and live as long as the host. Fetch and production run off every render loop, so animation and input never block on them.
    pub(in crate::sidebar_pane) fn start(
        config: &ServeConfig,
        runtime: &RuntimePaths,
        diag: &crate::diag::DiagSink,
    ) -> Self {
        let (mut plane, request_rx, observe_rx) = Self::idle();
        let observer = diag
            .is_enabled()
            .then(|| observe::RoomObserver::new(plane.observe_tx.clone()));
        plane.observe_events = observer.as_ref().map(|observer| observer.events.clone());
        let (config, runtime, diag) = (config.clone(), runtime.clone(), diag.clone());
        let subscribers = plane.subscribers.clone();
        let covered = plane.covered.clone();
        let lanes: Lanes = Box::new(move || {
            if diag.is_enabled() {
                observe::writer::spawn(runtime.clone(), diag.clone(), observe_rx);
            }
            spawn_fetch_worker(
                config.clone(),
                runtime.clone(),
                diag.clone(),
                request_rx,
                subscribers.clone(),
                covered,
                observer,
            );
            let template = config.clone();
            cache_refresh::spawn(config.clone(), runtime.clone(), diag, move || {
                lock(&subscribers)
                    .values()
                    .next()
                    .map(|pane| Stand::of(Some(pane), &template))
            });
            // tmux fast path: the host streams control-mode topology nudges so a pane open/close publishes a fresh pane frame in tens of milliseconds instead of waiting out the poll. Latency only: the poll stays the presence backstop, and Zellij reaches the same publication path through its presence plugin.
            if config.mux == MuxName::Tmux {
                tmux_watch::spawn(runtime.clone(), config.session_name.clone());
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
        let plane = Self {
            observe_events: None,
            caps: Arc::default(),
            geometry: Arc::default(),
            request_tx,
            subscribers: Subscribers::default(),
            covered: Covered::default(),
            observe_tx,
            lanes: Mutex::new(None),
        };
        (plane, request_rx, observe_rx)
    }

    /// A plane whose requests go nowhere, for tests that drive panes without a fold.
    #[cfg(test)]
    pub(in crate::sidebar_pane) fn detached() -> Self {
        Self::idle().0
    }

    /// Feed one more pane. The pane leaves the plane when the subscription drops.
    pub(in crate::sidebar_pane) fn subscribe(
        &self,
        config: &ServeConfig,
        socket_path: PathBuf,
    ) -> Subscription {
        let (tx, results) = result_channel();
        let subscriber = Subscriber {
            instance_id: config.instance_id.clone(),
            own_pane: config.own_pane.clone(),
            tick_seconds: config.tick_seconds,
            refresh_override: config.refresh_ms_override,
            tx,
            socket_path,
        };
        let membership = Membership {
            instance_id: config.instance_id.clone(),
            subscribers: self.subscribers.clone(),
        };
        let mut subscribers = lock(&self.subscribers);
        subscribers.insert(config.instance_id.as_str().to_owned(), subscriber);
        drop(subscribers);
        if let Some(start) = lock(&self.lanes).take() {
            start();
        }
        Subscription {
            requests: self.request_tx.clone(),
            covered: self.covered.clone(),
            results,
            observe: self.observe_tx.clone(),
            _membership: membership,
        }
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn subscriber_ids(&self) -> Vec<SidebarInstanceId> {
        lock(&self.subscribers)
            .values()
            .map(|subscriber| subscriber.instance_id.clone())
            .collect()
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane) fn publish_snapshot(
        &self,
        instance_id: &SidebarInstanceId,
        snapshot: crate::store::snapshot::SidebarSnapshot,
    ) {
        let waker = std::os::unix::net::UnixDatagram::unbound().unwrap();
        if let Some(subscriber) = lock(&self.subscribers).get(instance_id.as_str()) {
            subscriber
                .tx
                .send(FetchUpdate::Snapshot {
                    snapshot: Box::new(snapshot),
                    phase: super::fetch::FetchPhase::Final,
                    source: super::fetch::SnapshotSource::Produced,
                })
                .unwrap();
            waker.send_to(b"snapshot", &subscriber.socket_path).unwrap();
        }
    }
}

/// One pane's ends of the plane.
pub(in crate::sidebar_pane) struct Subscription {
    pub(super) requests: Sender<FetchRequest>,
    pub(super) covered: Covered,
    pub(super) results: ResultReceiver,
    pub(super) observe: SyncSender<ObserveMsg>,
    _membership: Membership,
}

struct Membership {
    instance_id: SidebarInstanceId,
    subscribers: Subscribers,
}

impl Drop for Membership {
    fn drop(&mut self) {
        let mut subscribers = lock(&self.subscribers);
        subscribers.remove(self.instance_id.as_str());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The subscriber set is only inserted into and removed from, and the lanes are only taken, so a panic elsewhere while either was held leaves it whole.
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}
