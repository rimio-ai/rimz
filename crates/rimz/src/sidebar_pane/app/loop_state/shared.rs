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

    pub(in crate::sidebar_pane::app) fn share_views(
        &mut self,
        views: Arc<Mutex<Views>>,
        revision: u64,
    ) {
        self.baseline_seed = self.ui.baseline_pane.take();
        self.views = views;
        self.own = super::super::views::Cursor {
            revision,
            accepted: None,
        };
        self.view_advanced = false;
    }

    pub(super) fn with_projected_view<T>(
        &mut self,
        input: bool,
        advancing: bool,
        advance: impl FnOnce(&mut Self) -> T,
    ) -> T {
        if self.view_scope.is_some() {
            return advance(self);
        }
        self.view_events = Some(
            self.fold_preparation
                .as_ref()
                .map_or_else(|| self.events().clone(), |prepared| prepared.events.clone()),
        );
        let Some(pane) = self.config.own_pane.as_ref() else {
            self.view_token = None;
            self.view_scope = Some(ViewScope::Scratch);
            let result = advance(self);
            self.view_scope = None;
            self.view_events = None;
            return result;
        };
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
            self.view_advanced |= advancing;
            let result = advance(self);
            self.view_scope = None;
            self.view_events = None;
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
        if advancing
            && entry.cursor.revision != revision
            && has_latest
            && self.fold_preparation.is_none()
        {
            drop(views);
            return self.view_transition(input, None, None, advance);
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
        let mut arrival_until = None;
        if advancing && (entry.unpainted || entry.last_pane.as_ref() != Some(&pane)) {
            if entry.last_pane.is_some() || self.view_advanced {
                entry.state.arrival_pending = true;
                context.tab_read_dwell_until = Some(now + crate::sidebar::timing::TAB_READ_DWELL);
                entry.state.viewing_own_tab = Some(true);
                arrival_until = context.tab_read_dwell_until;
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
        self.view_advanced |= advancing;
        std::mem::swap(&mut self.ui.view, &mut entry.state);
        std::mem::swap(&mut self.view, context);
        self.view_scope = Some(ViewScope::Shared(id));
        self.transition_view = Some(id);
        self.view_focus = focused;
        self.view_focus_event = focus_event;
        self.view_parked = parked;
        self.view_latest = has_latest;
        let mut reductions = super::super::views::Reductions::default();
        if advancing && self.fold_preparation.is_some() && entry.cursor.revision != revision {
            for latest in latest.since(entry.cursor.revision) {
                let clock_only = self.admits_clock_only(false);
                let update = entry.cursor.replay(
                    latest,
                    |fold| {
                        #[cfg(test)]
                        {
                            self.projections += 1;
                        }
                        Self::project_fold(fold, Some(&pane), self.config.refresh_ms_override)
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
                self.reduce_snapshot(update, false);
            }
            entry.cursor.revision = revision;
        }
        // Catch-up can replay hidden content older than the evidence that routed this arrival.
        if let Some(until) = arrival_until {
            self.view.tab_read_dwell_until = Some(until);
            self.ui.viewing_own_tab = Some(true);
        }
        self.view_token = Some((id, entry.cursor.revision, entry.activity));
        let result = advance(self);
        std::mem::swap(&mut self.ui.view, &mut entry.state);
        std::mem::swap(&mut self.view, context);
        self.ui.view = render::ViewState::default();
        if super::super::fetch::same_snapshot_content(&self.view.current, &context.current) {
            super::super::fetch::SnapshotClock::of(&context.current).apply(&mut self.view.current);
        } else {
            self.view.current.clone_from(&context.current);
        }
        views.reductions.content += reductions.content;
        views.reductions.unchanged += reductions.unchanged;
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
