//! Host-memory client membership and lazy presentation ownership.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use super::view_state::ViewContext;
use crate::ids::{MuxClientId, PaneId};
use crate::pane::ClientPaneView;
use crate::sidebar::frame::PaneFrame;
use crate::sidebar_pane::render::ViewState;
use crate::store::snapshot::SidebarSnapshot;

#[derive(Default)]
pub(super) struct Views {
    entries: BTreeMap<ViewId, Entry>,
    clients: BTreeMap<MuxClientId, Client>,
    panes: HashMap<PaneId, ViewId>,
    last_sample: Option<Vec<ClientRoute>>,
    retention: Option<Duration>,
    next_id: u64,
    activity: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct ViewId(u64);

struct Entry {
    state: ViewState,
    context: Option<ViewContext>,
    activity: u64,
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
                        && source.is_none_or(|source| activity[id] > activity[&source])
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
            if !panes.values().any(|painted| painted == id)
                && self.panes.values().any(|painted| painted == id)
                && !self.clients.values().any(|client| client.view == *id)
            {
                entry.unbound_since = Some(now);
            }
        }
        self.panes = panes;
        self.last_sample = Some(sample);
        self.expire(now, retention);
    }

    pub(super) fn paint(&mut self, pane: &PaneId, now: Instant) -> &mut ViewState {
        let id = self.route(pane, now);
        &mut self.entry_mut(id).state
    }

    pub(super) fn input(&mut self, pane: &PaneId, now: Instant) -> &mut ViewState {
        let id = self.route(pane, now);
        self.touch(id, now);
        &mut self.entry_mut(id).state
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
        self.next_id += 1;
        let id = ViewId(self.next_id);
        self.entries.insert(
            id,
            Entry {
                state,
                context,
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
