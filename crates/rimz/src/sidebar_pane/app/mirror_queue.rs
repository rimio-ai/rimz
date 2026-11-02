//! Ranked hidden paints share four slots and one host-wide input pause.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::ids::{PaneId, ViewId};

use super::timing::{MIRROR_POOL, MIRROR_QUIET};
use super::views::ViewsRecord;

// An attachment has many frame intervals to claim a wake before its slot is recycled.
const OFFER_LEASE: Duration = Duration::from_secs(4);

#[derive(Default)]
pub(super) struct MirrorQueue {
    state: Mutex<Queue>,
    changed: Condvar,
    subscribers: Mutex<super::fetch::Subscribers>,
}

#[derive(Default)]
struct Queue {
    record: Arc<ViewsRecord>,
    // Subscriber-backed state is pruned each step; offers plus grants are capped by MIRROR_POOL.
    subscribers: Vec<PaneId>,
    members: HashSet<PaneId>,
    target_changed: bool,
    pending: HashSet<PaneId>,
    flags: HashMap<PaneId, Arc<AtomicBool>>,
    offers: HashMap<PaneId, Instant>,
    grants: HashSet<PaneId>,
    quiet_until: Option<Instant>,
    current: Option<ViewId>,
    previous: Option<ViewId>,
    epoch: u64,
}

pub(super) struct Driver {
    queue: Arc<MirrorQueue>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

pub(super) struct Grant {
    queue: Arc<MirrorQueue>,
    pane: PaneId,
}

impl MirrorQueue {
    pub(super) fn new(subscribers: super::fetch::Subscribers) -> Self {
        Self {
            subscribers: Mutex::new(subscribers),
            ..Self::default()
        }
    }

    pub(super) fn bind(&self, subscribers: super::fetch::Subscribers) {
        *self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = subscribers;
        self.notify();
    }

    pub(super) fn notify(&self) {
        let mut state = self.lock();
        state.epoch += 1;
        self.changed.notify_one();
    }
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn flag(&self, pane: &PaneId) -> Arc<AtomicBool> {
        let subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let flag = subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .find(|subscriber| subscriber.own_pane == *pane)
            .map(|subscriber| subscriber.mirror_offer.clone());
        let mut state = self.lock();
        let Some(flag) = flag else {
            return state.flags.entry(pane.clone()).or_default().clone();
        };
        flag.store(state.offers.contains_key(pane), Ordering::Release);
        state.flags.insert(pane.clone(), flag.clone());
        if state.members.insert(pane.clone()) {
            state.subscribers.push(pane.clone());
        }
        state.epoch += 1;
        self.changed.notify_one();
        flag
    }

    pub(super) fn observe(&self, record: Arc<ViewsRecord>) {
        let mut state = self.lock();
        state.target_changed |= record.mirror_target != state.record.mirror_target;
        let current = record.mirror_pane.as_ref().and_then(|pane| {
            record
                .fold
                .as_ref()?
                .frame
                .as_ref()?
                .tabs
                .iter()
                .find(|tab| tab.panes.iter().any(|candidate| candidate.pane_id == *pane))
                .map(|tab| tab.view_id.clone())
        });
        if current.is_some() && current != state.current {
            state.previous = state.current.take();
            state.current = current;
        }
        state.record = record;
        state.epoch += 1;
        self.changed.notify_one();
    }

    pub(super) fn hold(&self, now: Instant) {
        let mut state = self.lock();
        state.quiet_until = Some(now + MIRROR_QUIET);
        if state.offers.is_empty() {
            return;
        }
        let offers = state.offers.keys().cloned().collect::<Vec<_>>();
        for pane in offers {
            state.clear_offer(&pane);
            state.pending.insert(pane);
        }
        state.epoch += 1;
        self.changed.notify_one();
    }

    pub(super) fn take(self: &Arc<Self>, pane: &PaneId, now: Instant) -> Option<Grant> {
        let mut state = self.lock();
        if !state.eligible(pane) {
            state.clear_offer(pane);
            state.epoch += 1;
            self.changed.notify_one();
            return None;
        }
        if state.members.contains(pane)
            && state.offers.get(pane).is_some_and(|until| now < *until)
            && state.quiet_until.is_none_or(|until| now >= until)
        {
            state.clear_offer(pane);
            state.pending.remove(pane);
            state.grants.insert(pane.clone());
            state.epoch += 1;
            self.changed.notify_one();
            return Some(Grant {
                queue: self.clone(),
                pane: pane.clone(),
            });
        }
        state.clear_offer(pane);
        state.pending.insert(pane.clone());
        state.epoch += 1;
        self.changed.notify_one();
        None
    }

    pub(super) fn discard_offer(&self, pane: &PaneId) {
        let mut state = self.lock();
        if !state.offers.contains_key(pane) {
            return;
        }
        state.clear_offer(pane);
        state.pending.remove(pane);
        state.epoch += 1;
        self.changed.notify_one();
    }

    #[cfg(test)]
    pub(super) fn step(
        &self,
        now: Instant,
        subscribers: Vec<PaneId>,
    ) -> (Vec<PaneId>, Option<Instant>) {
        self.lock().step(now, subscribers)
    }

    pub(super) fn dispatch(&self, now: Instant) -> (Option<Instant>, u64) {
        let subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let flags = subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|subscriber| (subscriber.own_pane.clone(), subscriber.mirror_offer.clone()))
            .collect::<Vec<_>>();
        let (wakes, deadline, epoch) = {
            let mut state = self.lock();
            for (pane, flag) in &flags {
                flag.store(state.offers.contains_key(pane), Ordering::Release);
                state.flags.insert(pane.clone(), flag.clone());
            }
            let panes = flags.into_iter().map(|(pane, _)| pane).collect();
            let (wakes, deadline) = state.step(now, panes);
            (wakes, deadline, state.epoch)
        };
        if !wakes.is_empty() {
            for subscriber in subscribers
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .values()
                .filter(|subscriber| wakes.contains(&subscriber.own_pane))
            {
                let _ = subscriber.wake.try_send(super::input::Wakeup::Tick);
            }
        }
        (deadline, epoch)
    }

    pub(super) fn spawn(self: &Arc<Self>) -> Driver {
        let queue = self.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || queue.run(&stopped));
        Driver {
            queue: self.clone(),
            stop,
            thread: Some(thread),
        }
    }

    fn run(&self, stop: &AtomicBool) {
        loop {
            let (deadline, epoch) = self.dispatch(Instant::now());
            let state = self.lock();
            if stop.load(Ordering::Acquire) {
                return;
            }
            if state.epoch != epoch {
                continue;
            }
            if let Some(deadline) = deadline {
                drop(
                    self.changed
                        .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                        .unwrap_or_else(PoisonError::into_inner),
                );
            } else {
                drop(
                    self.changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                );
            }
        }
    }
}

impl Drop for Grant {
    fn drop(&mut self) {
        let mut state = self.queue.lock();
        state.grants.remove(&self.pane);
        state.epoch += 1;
        self.queue.changed.notify_one();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.queue.notify();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Queue {
    fn eligible(&self, pane: &PaneId) -> bool {
        !self.record.mapped(pane) && !self.record.resolved(pane)
    }

    fn clear_offer(&mut self, pane: &PaneId) {
        self.offers.remove(pane);
        if let Some(flag) = self.flags.get(pane) {
            flag.store(false, Ordering::Release);
        }
    }

    fn sync_subscribers(&mut self, subscribers: Vec<PaneId>) {
        // Pending panes and offer flags never outlive their subscriber's next queue step.
        let eligible = subscribers
            .iter()
            .filter(|pane| self.eligible(pane))
            .cloned()
            .collect::<HashSet<_>>();
        self.pending.retain(|pane| eligible.contains(pane));
        self.members = subscribers.iter().cloned().collect();
        let retired = self
            .offers
            .keys()
            .filter(|pane| !eligible.contains(*pane))
            .cloned()
            .collect::<Vec<_>>();
        for pane in retired {
            self.clear_offer(&pane);
        }
        self.flags.retain(|pane, _| self.members.contains(pane));
        self.subscribers = subscribers;
    }

    fn step(&mut self, now: Instant, subscribers: Vec<PaneId>) -> (Vec<PaneId>, Option<Instant>) {
        self.sync_subscribers(subscribers);
        if std::mem::take(&mut self.target_changed) {
            if self.record.mirror_target.is_some() {
                let pending = self
                    .subscribers
                    .iter()
                    .filter(|pane| self.eligible(pane))
                    .cloned()
                    .collect::<Vec<_>>();
                self.pending.extend(pending);
            } else {
                self.pending.clear();
                let offers = self.offers.keys().cloned().collect::<Vec<_>>();
                for pane in offers {
                    self.clear_offer(&pane);
                }
            }
        }
        let expired = self
            .offers
            .iter()
            .filter(|(_, until)| now >= **until)
            .map(|(pane, _)| pane.clone())
            .collect::<Vec<_>>();
        for pane in expired {
            self.clear_offer(&pane);
        }
        let quiet = self.quiet_until.filter(|until| now < *until);
        let mut wakes = Vec::new();
        let slots = MIRROR_POOL.saturating_sub(self.offers.len() + self.grants.len());
        if quiet.is_none() && slots != 0 {
            for pane in self.ranked().into_iter().take(slots) {
                self.pending.remove(&pane);
                self.offers.insert(pane.clone(), now + OFFER_LEASE);
                self.flags
                    .entry(pane.clone())
                    .or_default()
                    .store(true, Ordering::Release);
                wakes.push(pane);
            }
        }
        let deadline = self
            .offers
            .values()
            .copied()
            .chain(quiet.filter(|_| !self.pending.is_empty()))
            .min();
        (wakes, deadline)
    }

    fn ranked(&self) -> Vec<PaneId> {
        let frame = self
            .record
            .fold
            .as_ref()
            .and_then(|fold| fold.frame.as_ref());
        let mut positions = frame
            .into_iter()
            .flat_map(|frame| frame.tabs.iter().filter_map(|tab| tab.position))
            .collect::<Vec<_>>();
        positions.sort_unstable();
        positions.dedup();
        let current = frame.and_then(|frame| {
            frame
                .tabs
                .iter()
                .find(|tab| Some(&tab.view_id) == self.current.as_ref())?
                .position
                .and_then(|position| positions.binary_search(&position).ok())
        });
        let mut ranks = HashMap::new();
        if let (Some(frame), Some(current)) = (frame, current) {
            for tab in &frame.tabs {
                let rank = tab
                    .position
                    .and_then(|position| positions.binary_search(&position).ok())
                    .map_or((4, 0, false), |ordinal| {
                        if ordinal.checked_add(1) == Some(current) {
                            return (0, 0, false);
                        }
                        if current.checked_add(1) == Some(ordinal) {
                            return (1, 0, false);
                        }
                        if Some(&tab.view_id) == self.previous.as_ref() {
                            return (2, 0, false);
                        }
                        (3, ordinal.abs_diff(current), ordinal > current)
                    });
                for pane in &tab.panes {
                    ranks.insert(&pane.pane_id, rank);
                }
            }
        }
        let mut pending = self
            .subscribers
            .iter()
            .enumerate()
            .filter(|(_, pane)| {
                self.pending.contains(*pane)
                    && !self.offers.contains_key(*pane)
                    && !self.grants.contains(*pane)
            })
            .map(|(index, pane)| {
                (
                    (ranks.get(pane).copied().unwrap_or((4, 0, false)), index),
                    pane.clone(),
                )
            })
            .collect::<Vec<_>>();
        pending.sort_by_key(|(rank, _)| *rank);
        pending.into_iter().map(|(_, pane)| pane).collect()
    }
}

#[cfg(test)]
mod tests;
