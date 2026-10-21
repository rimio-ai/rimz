//! Borrow the plane's presentation only during in-memory transitions.

use super::super::views::{ViewId, Views};
use super::*;

impl LoopState {
    pub(in crate::sidebar_pane::app) fn inspect_view<T>(
        &self,
        inspect: impl FnOnce(&render::ViewState, &ViewContext) -> T,
    ) -> T {
        if self.view_scope.is_none()
            && let Some(pane) = self.config.own_pane.as_ref()
        {
            let views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((state, context)) = views.peek(pane) {
                return inspect(state, context);
            }
        }
        inspect(&self.ui.view, &self.view)
    }

    pub(super) fn observed_watched(&self) -> bool {
        if self.shared_view_id().is_some() {
            return true;
        }
        if let Some(pane) = self.config.own_pane.as_ref() {
            let mut views = self.views.lock().unwrap_or_else(PoisonError::into_inner);
            views.refresh(self.clock.now());
            if views.resolved(pane) {
                return true;
            }
            if let Some(watched) = views.observed(pane) {
                return watched;
            }
        }
        self.watches_snapshot(&self.view.current)
    }

    pub(super) fn shared_view_id(&self) -> Option<ViewId> {
        match self.view_scope {
            Some(ViewScope::Shared(id)) => Some(id),
            _ => None,
        }
    }

    pub(in crate::sidebar_pane::app) fn share_views(&mut self, views: Arc<Mutex<Views>>) {
        self.baseline_seed = self.ui.baseline_pane.take();
        self.views = views;
    }

    pub(in crate::sidebar_pane::app) fn with_shared_view<T>(
        &mut self,
        input: bool,
        advancing: bool,
        advance: impl FnOnce(&mut Self) -> T,
    ) -> T {
        self.with_projected_view(input, advancing, None, advance)
    }

    pub(super) fn with_projected_view<T>(
        &mut self,
        input: bool,
        advancing: bool,
        delivered: Option<Arc<FetchUpdate>>,
        advance: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if self.view_scope.is_some() {
            return advance(self);
        }
        let Some(pane) = self.config.own_pane.as_ref() else {
            self.view_token = None;
            self.view_scope = Some(ViewScope::Scratch);
            let result = advance(self);
            self.view_scope = None;
            return result;
        };
        let events = self.view_events.take();
        let views = self.views.clone();
        let mut views = views.lock().unwrap_or_else(PoisonError::into_inner);
        let now = self.clock.now();
        views.refresh(now);
        let resolved = views.resolved(pane);
        if self.was_resolved && !resolved && !input {
            self.optimistic_watch_until = None;
        }
        self.was_resolved = resolved;
        let visible = self.optimistic_watch_until.is_some_and(|until| now < until)
            || views.observed(pane).unwrap_or_else(|| {
                self.fold_preparation
                    .as_ref()
                    .and_then(|prepared| prepared.visible)
                    .unwrap_or_else(|| self.watches_snapshot(&self.view.current))
            });
        if !input && !resolved && !visible {
            views.unpaint(pane, now);
            drop(views);
            self.view_token = None;
            self.view_scope = Some(ViewScope::Scratch);
            let result = advance(self);
            self.view_scope = None;
            return result;
        }
        let pane = pane.clone();
        let own_view = views.fold.as_ref().and_then(|fold| {
            fold.frame
                .as_deref()
                .and_then(|frame| SidebarOwnView::from_frame(&pane, frame))
        });
        // Unresolved copies keep their baseline, never an unrelated client's session focus.
        let focused = views.has_sample().then(|| views.focused(&pane));
        let parked = views.has_sample().then(|| views.parked(&pane));
        let focus_event = views.attributed_focus_event(&pane);
        let revision = views.revision;
        let (id, entry, latest) = views.catch_up(&pane, now, input);
        let has_latest = !latest.is_empty();
        if advancing && entry.revision != revision && has_latest && self.fold_preparation.is_none()
        {
            drop(views);
            return self.view_transition(input, None, None, delivered, advance);
        }
        if !advancing
            && entry.state.freeze.is_some()
            && self.fold_preparation.is_none()
            && events.is_none()
        {
            // Parking needs focus overlays; take their cut unlocked, then re-check routing.
            drop(views);
            self.view_events = Some(self.event_snapshot());
            return self.with_projected_view(input, advancing, delivered, advance);
        }
        if entry.context.is_none() {
            entry.context = Some(ViewContext::new(self.view.current.clone(), now));
            entry.state.make_up_filter = self.ui.make_up_filter.clone();
        }
        // Presentation initialization above guarantees the portable context exists.
        let context = entry.context.as_mut().expect("initialized view context");
        if let Some(own_view) = own_view {
            context.current.own_view = Some(own_view);
        }
        if advancing && (entry.unpainted || entry.last_pane.as_ref() != Some(&pane)) {
            if entry.last_pane.is_some() {
                entry.state.arrival_pending = true;
                context.tab_read_dwell_until = Some(now + crate::sidebar::timing::TAB_READ_DWELL);
                entry.state.viewing_own_tab = Some(true);
                if entry.state.freeze.is_none() {
                    layout_freeze::arm_freeze(
                        &mut entry.state,
                        self.clock.timestamp().as_millisecond(),
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
            entry.last_pane = Some(pane.clone());
            entry.unpainted = false;
        }
        std::mem::swap(&mut self.ui.view, &mut entry.state);
        std::mem::swap(&mut self.view, context);
        self.view_scope = Some(ViewScope::Shared(id));
        self.transition_view = Some(id);
        self.view_focus = focused;
        self.view_focus_event = focus_event;
        self.view_parked = parked;
        self.view_events = events;
        self.view_latest = has_latest;
        if advancing && self.fold_preparation.is_some() && entry.revision != revision {
            for latest in latest.since(entry.revision) {
                let full = delivered
                    .clone()
                    .filter(|delivered| {
                        let (
                            FetchUpdate::Shared { update, context },
                            FetchUpdate::Shared {
                                update: latest,
                                context: latest_context,
                            },
                        ) = (delivered.as_ref(), &latest)
                        else {
                            return false;
                        };
                        match (update.as_ref(), latest.as_ref()) {
                            (
                                FetchUpdate::Snapshot { phase, source, .. },
                                FetchUpdate::Fold {
                                    phase: latest_phase,
                                    source: latest_source,
                                    ..
                                },
                            ) => {
                                Arc::ptr_eq(context, latest_context)
                                    && phase == latest_phase
                                    && source == latest_source
                            }
                            _ => false,
                        }
                    })
                    .unwrap_or_else(|| {
                        let (update, shared) = latest.into_parts();
                        let update = match update {
                            FetchUpdate::Fold {
                                fold,
                                phase,
                                source,
                                ..
                            } => {
                                #[cfg(test)]
                                {
                                    self.projections += 1;
                                }
                                FetchUpdate::Snapshot {
                                    snapshot: Box::new(Self::project_fold(
                                        &fold,
                                        Some(&pane),
                                        self.config.refresh_ms_override,
                                    )),
                                    phase,
                                    source,
                                }
                            }
                            update => update,
                        };
                        Arc::new(match shared {
                            Some(context) => FetchUpdate::Shared {
                                update: Box::new(update),
                                context,
                            },
                            None => update,
                        })
                    });
                let update = if self.admits_clock_only(false)
                    && entry
                        .accepted
                        .as_ref()
                        .is_some_and(|accepted| full.same_content(accepted))
                {
                    FetchUpdate::unchanged(full.clone())
                } else {
                    (*full).clone()
                };
                self.reduce_snapshot(update, false);
                if full.is_success() {
                    entry.accepted = Some(full);
                }
            }
            entry.revision = revision;
        }
        self.view_token = Some((id, entry.revision, entry.activity));
        let result = advance(self);
        std::mem::swap(&mut self.ui.view, &mut entry.state);
        std::mem::swap(&mut self.view, context);
        self.ui.view = render::ViewState::default();
        if super::super::fetch::same_snapshot_content(&self.view.current, &context.current) {
            super::super::fetch::SnapshotClock::of(&context.current).apply(&mut self.view.current);
        } else {
            self.view.current.clone_from(&context.current);
        }
        self.view_scope = None;
        self.view_focus = None;
        self.view_focus_event = None;
        self.view_parked = None;
        self.view_events = None;
        self.view_latest = false;
        self.view_token = None;
        result
    }

    pub(super) fn retire_shared_anchor(
        &mut self,
        id: Option<ViewId>,
        anchor: &crate::mux::focus_anchor::FocusAnchor,
    ) {
        if let Some(id) = id {
            if let Some(context) = self
                .views
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .retained(id)
            {
                context.retire_anchor(anchor, self.clock.now());
            }
        } else {
            self.view.retire_anchor(anchor, self.clock.now());
        }
    }
}
