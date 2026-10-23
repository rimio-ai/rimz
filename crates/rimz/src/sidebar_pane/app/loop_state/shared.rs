//! Borrow the plane's presentation only during in-memory transitions.

use super::super::views::{ViewId, Views};
use super::*;

pub(in crate::sidebar_pane::app) struct ViewAccess {
    pub(in crate::sidebar_pane::app) registry: Arc<Mutex<Views>>,
    pub(in crate::sidebar_pane::app) events: Arc<Mutex<EventStore>>,
    scratch_state: ViewState,
    scratch_context: ViewContext,
    was_resolved: bool,
    view_advanced: bool,
    transition_view: Option<ViewId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViewTarget {
    Scratch,
    Shared(ViewId),
}

impl ViewTarget {
    pub(super) fn shared_id(self) -> Option<ViewId> {
        match self {
            Self::Scratch => None,
            Self::Shared(id) => Some(id),
        }
    }
}

pub(super) struct ViewBorrow<'a> {
    pub(super) state: &'a mut ViewState,
    pub(super) context: &'a mut ViewContext,
    pub(super) target: ViewTarget,
    pub(super) token: Option<(ViewId, u64, u64)>,
    pub(super) latest: bool,
    pub(super) focused: Option<Option<PaneId>>,
    pub(super) focus_event: Option<u64>,
    pub(super) parked: Option<bool>,
    pub(super) resolved: bool,
    pub(super) observed: Option<bool>,
    pub(super) events: EventStore,
}

pub(super) struct Route<'a> {
    input: bool,
    advancing: bool,
    prepared: Option<&'a FoldPreparation>,
}

impl<'a> Route<'a> {
    pub(super) const READ: Self = Self {
        input: false,
        advancing: false,
        prepared: None,
    };
    pub(super) const PAINT: Self = Self {
        input: false,
        advancing: true,
        prepared: None,
    };

    pub(super) fn fold(input: bool, prepared: &'a FoldPreparation) -> Self {
        Self {
            input,
            advancing: true,
            prepared: Some(prepared),
        }
    }
}

impl ViewAccess {
    pub(super) fn new(scratch_state: ViewState, scratch_context: ViewContext) -> Self {
        Self {
            registry: Arc::default(),
            events: Arc::default(),
            scratch_state,
            scratch_context,
            was_resolved: false,
            view_advanced: false,
            transition_view: None,
        }
    }

    pub(super) fn inspect<T>(
        &self,
        own_pane: Option<&PaneId>,
        inspect: impl FnOnce(&ViewState, &ViewContext) -> T,
    ) -> T {
        if let Some(pane) = own_pane {
            let views = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((state, context)) = views.peek(pane) {
                return inspect(state, context);
            }
        }
        inspect(&self.scratch_state, &self.scratch_context)
    }

    pub(in crate::sidebar_pane::app) fn mirror(&self) -> &SidebarSnapshot {
        &self.scratch_context.current
    }

    pub(super) fn visibility(
        &self,
        own_pane: Option<&PaneId>,
        now: impl FnOnce() -> Instant,
    ) -> (bool, Option<bool>) {
        let Some(pane) = own_pane else {
            return (false, None);
        };
        let mut views = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        views.refresh(now());
        (views.resolved(pane), views.observed(pane))
    }

    pub(super) fn share(&mut self, views: Arc<Mutex<Views>>) -> Option<PaneId> {
        self.registry = views;
        self.view_advanced = false;
        self.scratch_state.baseline_pane.take()
    }

    fn scratch(
        &mut self,
        events: EventStore,
        resolved: bool,
        observed: Option<bool>,
    ) -> ViewBorrow<'_> {
        ViewBorrow {
            state: &mut self.scratch_state,
            context: &mut self.scratch_context,
            target: ViewTarget::Scratch,
            token: None,
            latest: false,
            focused: None,
            focus_event: None,
            parked: None,
            resolved,
            observed,
            events,
        }
    }

    pub(super) fn with<T>(
        &mut self,
        pane: &mut PaneState,
        route: Route<'_>,
        advance: impl FnOnce(&mut PaneState, &mut ViewBorrow<'_>) -> T,
    ) -> T {
        let mut preparation = None;
        loop {
            let prepared = route.prepared.or(preparation.as_ref());
            let events = prepared.map_or_else(
                || {
                    self.events
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                },
                |prepared| prepared.events.clone(),
            );
            let Some(own_pane) = pane.config.own_pane.as_ref() else {
                return advance(pane, &mut self.scratch(events, false, None));
            };
            let mut views = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
            let now = pane.clock.now();
            views.refresh(now);
            let resolved = views.resolved(own_pane);
            if self.was_resolved && !resolved && !route.input {
                pane.optimistic_watch_until = None;
            }
            self.was_resolved = resolved;
            let observed = views.observed(own_pane);
            let visible = pane.optimistic_watch_until.is_some_and(|until| now < until)
                || observed.unwrap_or_else(|| {
                    prepared
                        .and_then(|prepared| prepared.visible)
                        .unwrap_or_else(|| pane.watches_snapshot(self.mirror()))
                });
            if !route.input && !resolved && !visible {
                views.unpaint(own_pane, now);
                drop(views);
                self.view_advanced |= route.advancing;
                return advance(pane, &mut self.scratch(events, resolved, observed));
            }
            let own_pane = own_pane.clone();
            let own_view = views.fold.as_ref().and_then(|fold| {
                fold.frame
                    .as_deref()
                    .and_then(|frame| SidebarOwnView::from_frame(&own_pane, frame))
            });
            // Unresolved copies keep their baseline, never an unrelated client's session focus.
            let focused = views.has_sample().then(|| views.focused(&own_pane));
            let parked = views.has_sample().then(|| views.parked(&own_pane));
            let focus_event = views.attributed_focus_event(&own_pane);
            let revision = views.revision;
            let (id, entry, latest) = views.catch_up(&own_pane, now, route.input);
            let has_latest = !latest.is_empty();
            if route.advancing
                && entry.cursor.revision != revision
                && has_latest
                && prepared.is_none()
            {
                drop(views);
                #[cfg(test)]
                {
                    pane.fold_preparations += 1;
                }
                let events = self
                    .events
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                preparation = Some(FoldPreparation::load(&pane.runtime, None, None, events));
                continue;
            }
            if entry.context.is_none() {
                entry.context = Some(ViewContext::new(self.scratch_context.current.clone(), now));
                entry.state.make_up_filter = self.scratch_state.make_up_filter.clone();
            }
            // Presentation initialization above guarantees the portable context exists.
            let context = entry.context.as_mut().expect("initialized view context");
            if let Some(own_view) = own_view {
                context.current.own_view = Some(own_view);
            }
            let mut arrival_until = None;
            if route.advancing && (entry.unpainted || entry.last_pane.as_ref() != Some(&own_pane)) {
                if entry.last_pane.is_some() || self.view_advanced {
                    entry.state.arrival_pending = true;
                    context.tab_read_dwell_until =
                        Some(now + crate::sidebar::timing::TAB_READ_DWELL);
                    entry.state.viewing_own_tab = Some(true);
                    arrival_until = context.tab_read_dwell_until;
                    if entry.state.freeze.is_none() {
                        layout_freeze::arm_freeze(
                            &mut entry.state,
                            pane.clock.timestamp().as_millisecond(),
                        );
                    }
                    if entry.unpainted {
                        entry.state.tally = Default::default();
                        entry.state.cost_rolls = Default::default();
                    }
                    if entry.state.scroll_ease.in_flight() {
                        entry.state.scroll_ease.cancel();
                        entry.state.offset_authoritative = true;
                    }
                }
                entry.last_pane = Some(own_pane.clone());
                entry.unpainted = false;
            }
            self.view_advanced |= route.advancing;
            self.transition_view = Some(id);
            let mut borrow = ViewBorrow {
                state: &mut entry.state,
                context,
                target: ViewTarget::Shared(id),
                token: None,
                latest: has_latest,
                focused,
                focus_event,
                parked,
                resolved,
                observed,
                events,
            };
            let mut reductions = super::super::views::Reductions::default();
            if route.advancing
                && let Some(prepared) = prepared.filter(|_| entry.cursor.revision != revision)
            {
                for latest in latest.since(entry.cursor.revision) {
                    let clock_only = pane.admits_clock_only(borrow.state, borrow.context, false);
                    let update = entry.cursor.replay(
                        latest,
                        |fold| {
                            #[cfg(test)]
                            {
                                pane.projections += 1;
                            }
                            LoopState::project_fold(
                                fold,
                                Some(&own_pane),
                                pane.config.refresh_ms_override,
                            )
                        },
                        clock_only,
                    );
                    if update.is_success() {
                        if update.is_unchanged() {
                            reductions.unchanged += 1;
                        } else {
                            reductions.content += 1;
                        }
                    }
                    pane.reduce_snapshot(&mut borrow, prepared, update, false);
                }
                entry.cursor.revision = revision;
            }
            // Catch-up can replay hidden content older than the evidence that routed this arrival.
            if let Some(until) = arrival_until {
                borrow.context.tab_read_dwell_until = Some(until);
                borrow.state.viewing_own_tab = Some(true);
            }
            borrow.token = Some((id, entry.cursor.revision, entry.activity));
            let result = advance(pane, &mut borrow);
            self.scratch_state = ViewState::default();
            if super::super::fetch::same_snapshot_content(self.mirror(), &context.current) {
                super::super::fetch::SnapshotClock::of(&context.current)
                    .apply(&mut self.scratch_context.current);
            } else {
                self.scratch_context.current.clone_from(&context.current);
            }
            views.reductions.content += reductions.content;
            views.reductions.unchanged += reductions.unchanged;
            return result;
        }
    }

    pub(super) fn take_transition(&mut self) -> Option<ViewId> {
        self.transition_view.take()
    }

    pub(super) fn retire_anchor(
        &mut self,
        id: Option<ViewId>,
        anchor: &crate::mux::focus_anchor::FocusAnchor,
        now: impl FnOnce() -> Instant,
    ) {
        if let Some(id) = id {
            if let Some(context) = self
                .registry
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retained(id)
            {
                context.retire_anchor(anchor, now());
            }
        } else {
            self.scratch_context.retire_anchor(anchor, now());
        }
    }

    pub(super) fn count_reduction(&self, unchanged: bool) {
        let mut views = self.registry.lock().unwrap_or_else(PoisonError::into_inner);
        if unchanged {
            views.reductions.unchanged += 1;
        } else {
            views.reductions.content += 1;
        }
    }

    #[cfg(test)]
    pub(in crate::sidebar_pane::app) fn scratch_for_test(
        &mut self,
    ) -> (&mut ViewState, &mut ViewContext) {
        (&mut self.scratch_state, &mut self.scratch_context)
    }

    #[cfg(test)]
    pub(super) fn rig(
        &mut self,
        own_pane: Option<&PaneId>,
        now: Instant,
    ) -> super::tests::RigView<'_> {
        let views = self.registry.lock().unwrap();
        let pane = own_pane.filter(|pane| views.peek(pane).is_some()).cloned();
        super::tests::RigView::new(
            views,
            pane,
            now,
            &mut self.scratch_state,
            &mut self.scratch_context,
        )
    }
}
