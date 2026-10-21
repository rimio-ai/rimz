//! Host-memory client membership and lazy presentation ownership.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
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
pub(super) struct Views {
    pub(super) latest: Latest,
    pub(super) fold: Option<Arc<WorkspaceFold>>,
    pub(super) revision: u64,
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) struct ViewId(u64);

/// The newest outcomes a lagging view replays, each tagged with its publication revision.
///
/// A view replays what it has not seen as a pane mailbox would deliver it: the
/// success before the failure, and an interim never in place of a completion the
/// view has not reduced.
#[derive(Default)]
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
        let unseen = |slot: &Option<(u64, FetchUpdate)>| {
            slot.as_ref()
                .filter(|(revision, _)| *revision > seen)
                .map(|(_, update)| update.clone())
        };
        let success = unseen(&self.completed).or_else(|| unseen(&self.interim));
        success.into_iter().chain(unseen(&self.failed)).collect()
    }
}

pub(super) struct Entry {
    pub(super) state: ViewState,
    pub(super) context: Option<ViewContext>,
    pub(super) revision: u64,
    pub(super) accepted: Option<Arc<FetchUpdate>>,
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
    pub(super) fn publish(&mut self, fold: Option<Arc<WorkspaceFold>>, update: FetchUpdate) {
        if let Some(fold) = fold {
            self.fold = Some(fold);
        }
        self.revision += 1;
        self.latest.push(self.revision, update);
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
        self.clients
            .values()
            .any(|client| client.departed.is_none() && client.pane.as_ref() == Some(pane))
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
        let fold = self.fold.as_ref()?;
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
        let (revision, accepted, last_pane, unpainted) =
            source.map_or((0, None, None, true), |id| {
                let source = &self.entries[&id];
                (
                    source.revision,
                    source.accepted.clone(),
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
                revision,
                accepted,
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
