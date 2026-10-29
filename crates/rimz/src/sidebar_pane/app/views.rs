//! Host-memory client membership and lazy presentation ownership.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::fetch::{FetchUpdate, WorkspaceFold};
use super::view_state::ViewContext;
use crate::ids::{MuxClientId, PaneId};
#[cfg(test)]
use crate::pane::ClientPaneView;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar_pane::render::ViewState;
#[cfg(test)]
use crate::store::snapshot::SidebarSnapshot;

#[derive(Default)]
pub(super) struct ViewsHandle {
    views: Mutex<Views>,
    record: Mutex<Arc<ViewsRecord>>,
    generation: AtomicU64,
}

impl From<Views> for ViewsHandle {
    fn from(views: Views) -> Self {
        Self {
            record: Mutex::new(Arc::new(ViewsRecord::new(&views, 0))),
            views: Mutex::new(views),
            generation: AtomicU64::new(0),
        }
    }
}

impl ViewsHandle {
    pub(super) fn read(&self) -> Arc<ViewsRecord> {
        self.record
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn release(&self, views: &Views) {
        let previous = self.read();
        if previous.matches(views) {
            return;
        }
        let generation = previous.generation + 1;
        let record = Arc::new(ViewsRecord::new(views, generation));
        *self.record.lock().unwrap_or_else(PoisonError::into_inner) = record;
        self.generation.store(generation, Ordering::Release);
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(super) fn lock(&self) -> ViewsGuard<'_> {
        #[cfg(any(test, feature = "testkit"))]
        crate::testkit::host_pass::count(crate::testkit::host_pass::Counter::ViewsLock);
        ViewsGuard {
            views: self.views.lock().unwrap_or_else(PoisonError::into_inner),
            handle: self,
        }
    }

    #[cfg(test)]
    pub(super) fn try_lock(&self) -> Option<ViewsGuard<'_>> {
        let views = match self.views.try_lock() {
            Ok(views) => views,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        crate::testkit::host_pass::count(crate::testkit::host_pass::Counter::ViewsLock);
        Some(ViewsGuard {
            views,
            handle: self,
        })
    }

    /// Leaves the handle as a release descheduled between its two stores does: the record
    /// one generation ahead of the published generation.
    #[cfg(test)]
    pub(super) fn install_ahead_of_generation(&self) {
        let views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
        let record = ViewsRecord::new(&views, self.read().generation + 1);
        *self.record.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(record);
    }
}

#[derive(Default)]
pub(super) struct ViewsRecord {
    pub(super) generation: u64,
    pub(super) revision: u64,
    pub(super) latest: Latest,
    fold: Option<Arc<WorkspaceFold>>,
    mapped_panes: HashSet<PaneId>,
    resolved_panes: HashSet<PaneId>,
}

impl ViewsRecord {
    fn new(views: &Views, generation: u64) -> Self {
        Self {
            generation,
            revision: views.revision,
            latest: views.latest.clone(),
            fold: views.fold.clone(),
            mapped_panes: views.panes.keys().cloned().collect(),
            resolved_panes: views.resolved_panes().cloned().collect(),
        }
    }

    fn matches(&self, views: &Views) -> bool {
        self.revision == views.revision
            && self.fold.as_ref().map(Arc::as_ptr) == views.fold.as_ref().map(Arc::as_ptr)
            && self.mapped_panes.len() == views.panes.len()
            && views.panes.keys().all(|pane| self.mapped(pane))
            && views.resolved_panes().all(|pane| self.resolved(pane))
            && self.resolved_panes.iter().all(|pane| views.resolved(pane))
    }

    pub(super) fn mapped(&self, pane: &PaneId) -> bool {
        self.mapped_panes.contains(pane)
    }

    pub(super) fn resolved(&self, pane: &PaneId) -> bool {
        self.resolved_panes.contains(pane)
    }

    pub(super) fn observed(&self, pane: &PaneId) -> Option<bool> {
        observed(self.fold.as_deref(), pane)
    }
}

fn observed(fold: Option<&WorkspaceFold>, pane: &PaneId) -> Option<bool> {
    let fold = fold?;
    let tab = fold
        .frame
        .as_ref()?
        .tabs
        .iter()
        .find(|tab| tab.panes.iter().any(|candidate| candidate.pane_id == *pane));
    Some(tab.is_none_or(|tab| {
        tab.panes.iter().any(|candidate| {
            fold.workspace
                .snapshot()
                .viewed_panes
                .contains(&candidate.pane_id)
                && (candidate.pane_id == *pane
                    || !candidate
                        .current
                        .command
                        .as_deref()
                        .is_some_and(crate::pane::command_is_sidebar_chrome))
        })
    }))
}

pub(super) struct ViewsGuard<'a> {
    views: MutexGuard<'a, Views>,
    handle: &'a ViewsHandle,
}

impl Drop for ViewsGuard<'_> {
    fn drop(&mut self) {
        self.handle.release(&self.views);
    }
}

impl Deref for ViewsGuard<'_> {
    type Target = Views;

    fn deref(&self) -> &Views {
        &self.views
    }
}

impl DerefMut for ViewsGuard<'_> {
    fn deref_mut(&mut self) -> &mut Views {
        &mut self.views
    }
}

/// The host's only fetch-content store, including replay and reduction baselines.
#[derive(Default)]
pub(super) struct Views {
    pub(super) latest: Latest,
    pub(super) fold: Option<Arc<WorkspaceFold>>,
    pub(super) revision: u64,
    pub(super) reductions: Reductions,
    sampled_frame: Option<Arc<PaneFrame>>,
    entries: BTreeMap<ViewId, Entry>,
    clients: BTreeMap<MuxClientId, Client>,
    panes: HashMap<PaneId, ViewId>,
    last_sample: Option<Vec<ClientRoute>>,
    retention: Option<Duration>,
    next_id: u64,
    activity: u64,
    sample_activity: u64,
    focus_event: Option<(u64, Vec<PaneId>)>,
    fold_waiter: Option<FoldWaiter>,
}

struct FoldWaiter {
    requested_at: Instant,
    reply: SyncSender<Result<Arc<WorkspaceFold>, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ViewId(u64);

/// The newest outcomes a lagging view replays, each tagged with its publication revision.
///
/// Views retain content once: success precedes failure, and an interim never
/// replaces a completion the consumer has not reduced.
#[derive(Clone, Default)]
pub(super) struct Latest {
    completed: Option<(u64, FetchUpdate)>,
    interim: Option<(u64, FetchUpdate)>,
    failed: Option<(u64, FetchUpdate)>,
}

impl Latest {
    fn push(&mut self, revision: u64, update: FetchUpdate) {
        if !update.is_success() {
            self.failed = Some((revision, update));
        } else if update.is_final() {
            self.completed = Some((revision, update));
            self.interim = None;
            self.failed = None;
        } else {
            self.interim = Some((revision, update));
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.completed.is_none() && self.interim.is_none() && self.failed.is_none()
    }

    /// Outcomes published after `seen`, in replay order.
    pub(super) fn since(&self, seen: u64) -> Vec<FetchUpdate> {
        self.unseen(seen).cloned().collect()
    }

    pub(super) fn unseen(&self, seen: u64) -> impl Iterator<Item = &FetchUpdate> {
        fn unseen(slot: &Option<(u64, FetchUpdate)>, seen: u64) -> Option<&FetchUpdate> {
            slot.as_ref()
                .filter(|(revision, _)| *revision > seen)
                .map(|(_, update)| update)
        }
        let success = unseen(&self.completed, seen).or_else(|| unseen(&self.interim, seen));
        success.into_iter().chain(unseen(&self.failed, seen))
    }
}

#[derive(Clone, Default)]
pub(super) struct Cursor {
    pub(super) revision: u64,
    pub(super) accepted: Option<Arc<FetchUpdate>>,
}

impl Cursor {
    pub(super) fn replay(
        &mut self,
        update: FetchUpdate,
        project: impl FnOnce(&WorkspaceFold) -> crate::store::snapshot::SidebarSnapshot,
        clock_only: bool,
    ) -> FetchUpdate {
        let (update, context) = update.into_parts();
        let update = match update {
            FetchUpdate::Fold {
                fold,
                phase,
                source,
            } => FetchUpdate::Snapshot {
                snapshot: Box::new(project(&fold)),
                phase,
                source,
            },
            update => update,
        };
        let full = Arc::new(match context {
            Some(context) => FetchUpdate::Shared {
                update: Box::new(update),
                context,
            },
            None => update,
        });
        let update = if clock_only
            && self
                .accepted
                .as_ref()
                .is_some_and(|prior| full.same_content(prior))
        {
            FetchUpdate::unchanged(full.clone())
        } else {
            (*full).clone()
        };
        if full.is_success() {
            self.accepted = Some(full);
        }
        update
    }
}

#[derive(Default)]
pub(super) struct Reductions {
    pub(super) content: u64,
    pub(super) unchanged: u64,
}

pub(super) struct Entry {
    pub(super) state: ViewState,
    pub(super) context: Option<ViewContext>,
    pub(super) cursor: Cursor,
    pub(super) last_pane: Option<PaneId>,
    pub(super) unpainted: bool,
    pub(super) activity: u64,
    /// Retention starts when the last client and painter leave.
    unbound_since: Option<Instant>,
}

#[derive(Clone)]
struct Client {
    view: ViewId,
    pane: Option<PaneId>,
    focused: PaneId,
    departed: Option<Instant>,
}

#[derive(Clone, PartialEq, Eq)]
struct ClientRoute {
    client_id: MuxClientId,
    pane: Option<PaneId>,
    focused: PaneId,
}

impl Views {
    pub(super) fn fresh_fold(&self, now: Instant, tick: Duration) -> Option<Arc<WorkspaceFold>> {
        self.fold
            .as_ref()
            .filter(|fold| now.saturating_duration_since(fold.sampled_at) < tick)
            .cloned()
    }

    /// One refresher waits at a time; retain its first qualifying outcome, not a queue.
    pub(super) fn wait_for_fold(
        &mut self,
        requested_at: Instant,
    ) -> Receiver<Result<Arc<WorkspaceFold>, String>> {
        let (reply, receiver) = sync_channel(1);
        self.fold_waiter = Some(FoldWaiter {
            requested_at,
            reply,
        });
        receiver
    }

    pub(super) fn publish(&mut self, update: FetchUpdate, now: Instant) {
        let content = match &update {
            FetchUpdate::Shared { update, .. } => update.as_ref(),
            update => update,
        };
        if let FetchUpdate::Fold { fold, .. } = content {
            self.fold = Some(fold.clone());
        }
        let reply = self.fold_waiter.as_ref().and_then(|waiter| match content {
            FetchUpdate::Fold { fold, .. } if fold.sampled_at > waiter.requested_at => {
                Some(Ok(fold.clone()))
            }
            FetchUpdate::Failed { error } => Some(Err(error.clone())),
            _ => None,
        });
        self.revision += 1;
        self.latest.push(self.revision, update);
        self.refresh(now);
        if let Some(reply) = reply
            && let Some(waiter) = self.fold_waiter.take()
        {
            let _ = waiter.reply.try_send(reply);
        }
    }

    pub(super) fn refresh(&mut self, now: Instant) {
        if let Some(fold) = &self.fold {
            let retention = Duration::from_millis(fold.workspace.snapshot().sidebar.afk_after_ms());
            if let Some(frame) = fold.frame.clone()
                && self
                    .sampled_frame
                    .as_ref()
                    .is_none_or(|old| !Arc::ptr_eq(old, &frame))
            {
                self.reconcile_frame(&frame, now, retention);
                self.sampled_frame = Some(frame);
            }
            self.retention = Some(retention);
        }
        if let Some(retention) = self.retention {
            self.expire(now, retention);
        }
    }

    pub(super) fn resolved(&self, pane: &PaneId) -> bool {
        self.resolved_panes().any(|resolved| resolved == pane)
    }

    fn resolved_panes(&self) -> impl Iterator<Item = &PaneId> {
        self.clients
            .values()
            .filter(|client| client.departed.is_none())
            .filter_map(|client| client.pane.as_ref())
    }

    pub(super) fn has_sample(&self) -> bool {
        self.last_sample.is_some()
    }

    pub(super) fn note_focus_event(&mut self, focused: &[PaneId], unfocused: &[PaneId], sent: u64) {
        if self
            .focus_event
            .as_ref()
            .is_none_or(|(stamp, _)| sent >= *stamp)
        {
            self.focus_event = Some((sent, focused.iter().chain(unfocused).cloned().collect()));
        }
    }

    pub(super) fn attributed_focus_event(&self, pane: &PaneId) -> Option<u64> {
        let (stamp, panes) = self.focus_event.as_ref()?;
        if pane.mux() == crate::MuxName::Tmux || !self.has_sample() {
            return Some(*stamp);
        }
        let id = self.panes.get(pane)?;
        let frame = self.fold.as_ref()?.frame.as_ref()?;
        let mut witnessed = false;
        for tab in frame.tabs.iter().filter(|tab| {
            tab.panes
                .iter()
                .any(|candidate| panes.contains(&candidate.pane_id))
        }) {
            for client in self.clients.values().filter(|client| {
                client.departed.is_none()
                    && tab
                        .panes
                        .iter()
                        .any(|candidate| candidate.pane_id == client.focused)
            }) {
                witnessed = true;
                if client.view != *id {
                    return None;
                }
            }
        }
        witnessed.then_some(*stamp)
    }

    /// Whether the latest frame shows `pane`'s tab viewed; a frameless fold observes nothing.
    pub(super) fn observed(&self, pane: &PaneId) -> Option<bool> {
        observed(self.fold.as_deref(), pane)
    }

    #[cfg(test)]
    pub(super) fn presentation(
        &mut self,
        pane: &PaneId,
        now: Instant,
        input: bool,
    ) -> (ViewId, &mut Entry) {
        let (id, entry, _) = self.catch_up(pane, now, input);
        (id, entry)
    }

    /// Route `pane` to its view, with the outcomes that view has not reduced yet.
    pub(super) fn catch_up(
        &mut self,
        pane: &PaneId,
        now: Instant,
        input: bool,
    ) -> (ViewId, &mut Entry, &Latest) {
        let id = self.route(pane, now);
        if input {
            self.touch(id, now);
        }
        // IDs originate in `fork`; expiry removes their routing references in the same transition.
        let entry = self.entries.get_mut(&id).expect("routed view exists");
        (id, entry, &self.latest)
    }

    /// The retained view whose dispatched jump targets `target`.
    pub(super) fn dispatched(
        &mut self,
        target: &PaneId,
    ) -> Option<(&mut ViewState, &mut ViewContext)> {
        self.entries.values_mut().find_map(|entry| {
            let Entry { state, context, .. } = entry;
            let context = context.as_mut()?;
            let dispatched = context
                .focus_rollback
                .as_ref()
                .is_some_and(|rollback| rollback.target == *target);
            dispatched.then_some((state, context))
        })
    }

    pub(super) fn retained(&mut self, id: ViewId) -> Option<&mut ViewContext> {
        self.entries.get_mut(&id)?.context.as_mut()
    }

    pub(super) fn peek(&self, pane: &PaneId) -> Option<(&ViewState, &ViewContext)> {
        let entry = self.entries.get(self.panes.get(pane)?)?;
        Some((&entry.state, entry.context.as_ref()?))
    }

    pub(super) fn unpaint(&mut self, pane: &PaneId, now: Instant) {
        if self.resolved(pane) {
            return;
        }
        let Some(id) = self.panes.remove(pane) else {
            return;
        };
        let entry = self.entry_mut(id);
        entry.unpainted = true;
        entry.unbound_since = Some(now);
        if let Some(context) = &mut entry.context {
            context.tab_read_dwell_until = None;
        }
    }

    pub(super) fn reconcile_frame(&mut self, frame: &PaneFrame, now: Instant, retention: Duration) {
        if !frame.client_view_fresh {
            self.retention = Some(retention);
            return;
        }
        let clients: Vec<_> = frame
            .client_views
            .iter()
            .map(|client| ClientRoute {
                client_id: client.client_id.clone(),
                pane: frame
                    .tabs
                    .iter()
                    .find(|tab| tab.panes.iter().any(|pane| pane.pane_id == client.pane_id))
                    .and_then(|tab| {
                        let focused = tab
                            .panes
                            .iter()
                            .find(|pane| pane.pane_id == client.pane_id)?;
                        if focused
                            .current
                            .command
                            .as_deref()
                            .is_some_and(crate::pane::command_is_sidebar_chrome)
                        {
                            return Some(focused);
                        }
                        tab.panes.iter().find(|pane| {
                            pane.current
                                .command
                                .as_deref()
                                .is_some_and(crate::pane::command_is_sidebar_chrome)
                        })
                    })
                    .map(|pane| pane.pane_id.clone()),
                focused: client.pane_id.clone(),
            })
            .collect();
        self.reconcile_routes(&clients, true, now, retention);
    }

    pub(super) fn focused(&self, pane: &PaneId) -> Option<PaneId> {
        let id = *self.panes.get(pane)?;
        let mut focused = self
            .clients
            .values()
            .filter(|client| client.view == id && client.departed.is_none())
            .map(|client| &client.focused);
        let first = focused.next()?;
        (first != pane && focused.all(|focus| focus == first)).then(|| first.clone())
    }

    pub(super) fn parked(&self, pane: &PaneId) -> bool {
        self.panes.get(pane).is_some_and(|id| {
            self.clients.values().any(|client| {
                client.view == *id && client.departed.is_none() && &client.focused == pane
            })
        })
    }

    #[cfg(test)]
    pub(super) fn context(
        &mut self,
        pane: &PaneId,
        now: Instant,
        initial: SidebarSnapshot,
    ) -> &mut ViewContext {
        let id = self.route(pane, now);
        self.entry_mut(id)
            .context
            .get_or_insert_with(|| ViewContext::new(initial, now))
    }

    #[cfg(test)]
    pub(super) fn reconcile(
        &mut self,
        clients: &[ClientPaneView],
        fresh: bool,
        now: Instant,
        retention: Duration,
    ) {
        let clients: Vec<_> = clients
            .iter()
            .map(|client| ClientRoute {
                client_id: client.client_id.clone(),
                pane: Some(client.pane_id.clone()),
                focused: client.pane_id.clone(),
            })
            .collect();
        self.reconcile_routes(&clients, fresh, now, retention);
    }

    fn reconcile_routes(
        &mut self,
        clients: &[ClientRoute],
        fresh: bool,
        now: Instant,
        retention: Duration,
    ) {
        self.retention = Some(retention);
        if !fresh {
            return;
        }
        self.expire(now, retention);
        let mut sample = clients.to_vec();
        sample.sort_by(|left, right| left.client_id.cmp(&right.client_id));
        if self.last_sample.as_ref() == Some(&sample) {
            return;
        }

        let old_clients = self.clients.clone();
        let activity: BTreeMap<_, _> = self
            .entries
            .iter()
            .map(|(&id, entry)| (id, entry.activity))
            .collect();
        let most_recent = self.most_recent(|_| true);
        let mut destinations: HashMap<PaneId, Vec<MuxClientId>> = HashMap::new();
        for client in &sample {
            let Some(pane) = &client.pane else {
                continue;
            };
            destinations
                .entry(pane.clone())
                .or_default()
                .push(client.client_id.clone());
        }
        let mut destinations: Vec<_> = destinations.into_iter().collect();
        destinations.sort_by(|(_, left), (_, right)| {
            let priority = |clients: &[MuxClientId]| {
                clients
                    .iter()
                    .filter_map(|client| old_clients.get(client))
                    .map(|client| activity[&client.view])
                    .max()
                    .unwrap_or(0)
            };
            priority(right)
                .cmp(&priority(left))
                .then_with(|| left[0].cmp(&right[0]))
        });

        let mut panes = HashMap::new();
        let mut assigned = HashSet::new();
        // Continuing viewers reserve their destination before any arrival can take it.
        for (pane, clients) in &destinations {
            if let Some(client) = clients.iter().find_map(|id| {
                old_clients
                    .get(id)
                    .filter(|client| client.pane.as_ref() == Some(pane))
            }) {
                panes.insert(pane.clone(), client.view);
                assigned.insert(client.view);
            }
        }
        for (pane, clients) in &destinations {
            if let Some(&view) = panes.get(pane) {
                if clients.iter().any(|id| {
                    old_clients
                        .get(id)
                        .is_none_or(|client| client.pane.as_ref() != Some(pane))
                }) {
                    self.touch(view, now);
                }
                continue;
            }
            let source = clients
                .iter()
                .filter_map(|id| old_clients.get(id).map(|client| (id, client.view)))
                .max_by(|(left_client, left), (right_client, right)| {
                    activity[left]
                        .cmp(&activity[right])
                        .then_with(|| right_client.cmp(left_client))
                })
                .map(|(_, view)| view)
                .or(most_recent);
            let source = self
                .panes
                .get(pane)
                .copied()
                .filter(|id| {
                    !old_clients.values().any(|client| client.view == *id)
                        && activity[id] > self.sample_activity
                })
                .or(source);
            let view = match source {
                Some(source)
                    if !assigned.contains(&source)
                        && !old_clients.iter().any(|(id, client)| {
                            client.view == source
                                && client.departed.is_some()
                                && !clients.contains(id)
                        }) =>
                {
                    source
                }
                source => self.fork(source, now),
            };
            self.touch(view, now);
            panes.insert(pane.clone(), view);
            assigned.insert(view);
        }

        let mut next_clients = BTreeMap::new();
        for client in &sample {
            let view = if let Some(pane) = &client.pane {
                panes[pane]
            } else {
                let source = old_clients
                    .get(&client.client_id)
                    .map(|client| client.view)
                    .or(most_recent);
                match source {
                    Some(id)
                        if !assigned.contains(&id)
                            && !old_clients
                                .values()
                                .any(|client| client.view == id && client.departed.is_some()) =>
                    {
                        id
                    }
                    source => self.fork(source, now),
                }
            };
            assigned.insert(view);
            self.entry_mut(view).unbound_since = None;
            next_clients.insert(
                client.client_id.clone(),
                Client {
                    view,
                    pane: client.pane.clone(),
                    focused: client.focused.clone(),
                    departed: None,
                },
            );
        }
        let mut departed_copies = HashMap::new();
        for (id, mut client) in old_clients {
            if next_clients.contains_key(&id) {
                continue;
            }
            if client.departed.is_none() {
                if assigned.contains(&client.view) {
                    client.view = *departed_copies
                        .entry(client.view)
                        .or_insert_with(|| self.fork(Some(client.view), now));
                }
                client.departed = Some(now);
                client.pane = None;
                self.entry_mut(client.view).unbound_since = Some(now);
            }
            next_clients.insert(id, client);
        }
        self.clients = next_clients;
        for (id, entry) in &mut self.entries {
            if !panes.values().any(|painted| painted == id) {
                if self.panes.values().any(|painted| painted == id)
                    && !self.clients.values().any(|client| client.view == *id)
                {
                    entry.unbound_since = Some(now);
                }
                entry.unpainted = true;
                if let Some(context) = &mut entry.context {
                    context.tab_read_dwell_until = None;
                }
            }
        }
        self.panes = panes;
        self.last_sample = Some(sample);
        self.sample_activity = self.activity;
        self.expire(now, retention);
    }

    #[cfg(test)]
    pub(super) fn paint(&mut self, pane: &PaneId, now: Instant) -> &mut ViewState {
        let id = self.route(pane, now);
        &mut self.entry_mut(id).state
    }

    #[cfg(test)]
    pub(super) fn input(&mut self, pane: &PaneId, now: Instant) -> &mut ViewState {
        let id = self.route(pane, now);
        self.touch(id, now);
        &mut self.entry_mut(id).state
    }

    pub(super) fn land(&mut self, source: ViewId, pane: &PaneId, now: Instant) {
        self.refresh(now);
        if self.resolved(pane) || !self.entries.contains_key(&source) {
            return;
        }
        let held_elsewhere = self
            .panes
            .iter()
            .any(|(held_pane, id)| held_pane != pane && *id == source);
        let departed = self
            .clients
            .values()
            .any(|client| client.view == source && client.departed.is_some());
        let view = if held_elsewhere || departed {
            self.fork(Some(source), now)
        } else {
            source
        };
        if self.panes.get(pane).is_some_and(|current| *current != view) {
            self.unpaint(pane, now);
        }
        self.touch(view, now);
        self.panes.insert(pane.clone(), view);
    }

    fn route(&mut self, pane: &PaneId, now: Instant) -> ViewId {
        if let Some(retention) = self.retention {
            self.expire(now, retention);
        }
        if let Some(&view) = self.panes.get(pane) {
            return view;
        }
        let attached: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, client)| client.departed.is_none())
            .map(|(id, client)| (id.clone(), client.view, client.pane.clone()))
            .collect();
        let view = if let [(client, view, None)] = attached.as_slice() {
            let view = *view;
            self.panes.retain(|_, held| *held != view);
            // `attached` was collected from this unchanged client map above.
            self.clients.get_mut(client).expect("attached client").pane = Some(pane.clone());
            view
        } else if let Some(view) =
            self.most_recent(|id| !self.panes.values().any(|held| *held == id))
        {
            if self
                .clients
                .values()
                .any(|client| client.view == view && client.departed.is_some())
            {
                self.fork(Some(view), now)
            } else {
                view
            }
        } else {
            self.fork(self.most_recent(|_| true), now)
        };
        self.touch(view, now);
        self.panes.insert(pane.clone(), view);
        view
    }

    fn most_recent(&self, include: impl Fn(ViewId) -> bool) -> Option<ViewId> {
        self.entries
            .iter()
            .filter(|(id, _)| include(**id))
            .max_by_key(|(id, entry)| (entry.activity, **id))
            .map(|(&id, _)| id)
    }

    fn fork(&mut self, source: Option<ViewId>, now: Instant) -> ViewId {
        let state = source
            .map(|id| self.entries[&id].state.clone())
            .unwrap_or_default();
        let activity = source.map_or(0, |id| self.entries[&id].activity);
        let context = source.and_then(|id| self.entries[&id].context.clone());
        let (cursor, last_pane, unpainted) = source.map_or((Cursor::default(), None, true), |id| {
            let source = &self.entries[&id];
            (
                source.cursor.clone(),
                source.last_pane.clone(),
                source.unpainted,
            )
        });
        self.next_id += 1;
        let id = ViewId(self.next_id);
        self.entries.insert(
            id,
            Entry {
                state,
                context,
                cursor,
                last_pane,
                unpainted,
                activity,
                unbound_since: Some(now),
            },
        );
        id
    }

    fn touch(&mut self, id: ViewId, now: Instant) {
        self.activity += 1;
        let activity = self.activity;
        let entry = self.entry_mut(id);
        entry.activity = activity;
        if entry.unbound_since.is_some() {
            entry.unbound_since = Some(now);
        }
    }

    fn expire(&mut self, now: Instant, retention: Duration) {
        self.clients.retain(|_, client| {
            client
                .departed
                .is_none_or(|departed| now.saturating_duration_since(departed) < retention)
        });
        self.entries.retain(|id, entry| {
            self.clients.values().any(|client| client.view == *id)
                || self.panes.values().any(|painted| painted == id)
                || entry
                    .unbound_since
                    .is_some_and(|since| now.saturating_duration_since(since) < retention)
        });
        self.panes.retain(|_, view| self.entries.contains_key(view));
    }

    fn entry_mut(&mut self, id: ViewId) -> &mut Entry {
        // IDs originate in `fork`; expiry removes their routing references in the same transition.
        self.entries.get_mut(&id).expect("routed view exists")
    }
}

#[cfg(test)]
mod tests;
