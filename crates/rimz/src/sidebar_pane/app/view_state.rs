//! Portable reconciliation state advanced by the attachment painting a view.

use std::sync::Arc;
use std::time::Instant;

use jiff::Timestamp;

use crate::MuxName;
use crate::diag::record::DiagEvent;
use crate::ids::{FocusNonce, PaneId};
use crate::mux::focus_anchor::{
    FocusAnchor, FocusObservation, FocusObservationOutcome, FocusOrigin, FocusPresentation,
};
use crate::sidebar::event_store::EventStore;
use crate::sidebar::fuse::{focus_intent_confirmed_from, fuse_owned};
use crate::sidebar::observe;
use crate::sidebar::read_marks::ReadMarks;
use crate::sidebar::timing::{FOCUS_MARKER_TMUX_DWELL, FOCUS_MARKER_ZELLIJ_DWELL, TAB_READ_DWELL};
use crate::sidebar::unread::UnreadClearCause;
use crate::sidebar_pane::render::{self, ViewState};
use crate::store::snapshot::SidebarSnapshot;

use super::clock::LoopClock;
use super::fetch::{FetchUpdate, FoldInputs, SnapshotSource};
use super::gate::{GateState, apply_gate};
use super::health::Health;
use super::layout_freeze;
use super::loop_state::own_tab_viewed;
use super::selection::{reconcile_selection, set_make_up_filter};
use super::state::{
    FetchDiagnostics, ReadClear, apply_manual_unread_guard, compute_next_state, fetch_diagnostics,
    read_receipt_for_row, read_receipts_for_tab, row_id_of_pane, session_focus_baseline,
    set_rows_unread,
};
use super::timing::frame_clock;

pub(super) struct ReadContext<'a> {
    pub(super) own_pane: &'a PaneId,
    pub(super) marks: &'a ReadMarks,
    pub(super) now: Timestamp,
    pub(super) at: Instant,
}

pub(super) struct SelectionFold<'a> {
    pub(super) previous: &'a SidebarSnapshot,
    pub(super) selected: Option<PaneId>,
    pub(super) focused: Option<PaneId>,
    pub(super) cleared: bool,
    pub(super) filter: Option<crate::sidebar_pane::view::BodyLens>,
    pub(super) mux: MuxName,
    pub(super) clock: &'a LoopClock,
    pub(super) now: Instant,
    pub(super) parked: bool,
}

#[derive(Clone, Copy)]
pub(super) enum FocusMarker {
    Dwelling(Instant),
    Ready,
}

#[derive(Clone)]
pub(super) struct FocusRollback {
    pub(super) target: PaneId,
    pub(super) baseline: Option<PaneId>,
    pub(super) label: String,
}

#[derive(Clone)]
pub(super) struct ViewContext {
    pub(super) current: SidebarSnapshot,
    pub(super) anim_start: Instant,
    /// Full pulled truth retained only while an overlay, focus fence, or gate
    /// hold can outlive its source. The steady overlay-free path moves the pull
    /// directly into `current` and keeps only the compact projections below.
    pub(super) overlay_baseline: Option<SidebarSnapshot>,
    pub(super) last_focus_observation: FocusObservation,
    pub(super) observation: Option<Arc<observe::FrameSig>>,
    pub(super) fold_inputs: Option<Arc<FoldInputs>>,
    pub(super) observed_anchor: Option<(Instant, Option<FocusAnchor>)>,
    pub(super) last_folded_focus_intent: Option<FocusNonce>,
    pub(super) live_focus_source: Option<FocusNonce>,
    pub(super) focus_landing: Option<(FocusAnchor, Instant)>,
    // Intent adoption can replace the hold-last baseline before dispatch fails.
    pub(super) focus_rollback: Option<FocusRollback>,
    pub(super) pending_input_focus: Option<PaneId>,
    pub(super) focus_marker: Option<FocusMarker>,
    pub(super) retired_anchor: Option<(Instant, u64)>,
    pub(super) filter_observed_at: Option<Instant>,
    pub(super) last_pulled_rows: usize,
    /// Deadline for the tab-view read sweep: armed when the own tab comes on
    /// screen, disarmed when it leaves. The sweep fires on the first fold at or
    /// past it while the tab is still viewed, so a pass-through never clears
    /// siblings. `None` when no dwell is pending.
    pub(super) tab_read_dwell_until: Option<Instant>,
    pub(super) confirmed_focus_intent_ms: u64,
    pub(super) health: Health,
    pub(super) gate: GateState,
}

impl ViewContext {
    pub(super) fn new(current: SidebarSnapshot, now: Instant) -> Self {
        Self {
            last_focus_observation: FocusObservation::from_snapshot(&current),
            current,
            anim_start: now,
            overlay_baseline: None,
            observation: None,
            fold_inputs: None,
            observed_anchor: None,
            last_folded_focus_intent: None,
            live_focus_source: None,
            focus_landing: None,
            focus_rollback: None,
            pending_input_focus: None,
            focus_marker: None,
            retired_anchor: None,
            filter_observed_at: None,
            last_pulled_rows: 0,
            tab_read_dwell_until: None,
            confirmed_focus_intent_ms: 0,
            health: Health::default(),
            gate: GateState::default(),
        }
    }

    pub(super) fn observe_focus_anchor(
        &mut self,
        session: &str,
        observed: Instant,
        loaded: Option<FocusAnchor>,
        own_pane: Option<&PaneId>,
    ) -> Option<FocusAnchor> {
        let accepts = |anchor: &FocusAnchor| {
            anchor.session_name == session
                && own_pane.is_none_or(|own| {
                    own == &anchor.pane_id
                        || self
                            .current
                            .own_view
                            .as_ref()
                            .is_some_and(|view| view.working_pane_ids.contains(&anchor.pane_id))
                        || self.pending_input_focus.as_ref() == Some(&anchor.pane_id)
                        || self.live_focus_source == Some(anchor.nonce)
                })
        };
        if let Some(inputs) = &self.fold_inputs {
            if let Some((observed, anchor)) = &self.observed_anchor
                && inputs.sampled_at <= *observed
            {
                return anchor.as_ref().filter(|anchor| accepts(anchor)).cloned();
            }
            let anchor = inputs
                .anchor
                .as_ref()
                .filter(|anchor| accepts(anchor))
                .cloned();
            self.observed_anchor = Some((inputs.sampled_at, anchor.clone()));
            return anchor;
        }
        let anchor = loaded.filter(accepts);
        if anchor.is_none()
            && let Some((_, Some(previous))) = &self.observed_anchor
        {
            self.retired_anchor = Some((observed, previous.issued_at_ms));
        }
        self.observed_anchor = Some((observed, anchor.clone()));
        anchor
    }

    pub(super) fn pending_focus_intent(
        &mut self,
        anchor: Option<&FocusAnchor>,
        events: &EventStore,
        now_ms: u64,
    ) -> (Option<FocusPresentation>, Option<FocusObservationOutcome>) {
        let Some(anchor) = anchor else {
            // A live retirement must fence an older cut's focus observation,
            // not revive either the intent or the pre-jump selection.
            let presentation = self
                .fold_inputs
                .as_ref()
                .filter(|inputs| {
                    self.retired_anchor
                        .as_ref()
                        .is_some_and(|(retired, issued_at_ms)| {
                            inputs.sampled_at <= *retired
                                && !events.has_focus_since(*issued_at_ms, now_ms)
                        })
                })
                .map(|_| FocusPresentation::Fence);
            return (presentation, None);
        };
        let outcome =
            if focus_intent_confirmed_from(&self.last_focus_observation, events, anchor, now_ms) {
                FocusObservationOutcome::Confirmed
            } else {
                crate::mux::focus_anchor::observation_outcome_from(
                    anchor,
                    &self.last_focus_observation,
                    now_ms,
                )
            };
        match outcome {
            FocusObservationOutcome::Present => (
                Some(FocusPresentation::Target(Box::new(anchor.clone()))),
                None,
            ),
            FocusObservationOutcome::Fence => (Some(FocusPresentation::Fence), None),
            FocusObservationOutcome::Confirmed
            | FocusObservationOutcome::Superseded
            | FocusObservationOutcome::Invalidated => {
                if outcome == FocusObservationOutcome::Confirmed
                    && anchor.origin == FocusOrigin::User
                {
                    self.confirmed_focus_intent_ms = anchor.issued_at_ms;
                    return (None, None);
                }
                (None, Some(outcome))
            }
        }
    }

    pub(super) fn apply_focus_anchor(
        &mut self,
        ui: &mut ViewState,
        anchor: &FocusAnchor,
        clock: &LoopClock,
    ) -> bool {
        let now_ms = clock.now_ms();
        let presentation_at_ms = anchor.applied_at_ms.unwrap_or(anchor.issued_at_ms);
        if self
            .current
            .row_owning_pane(&anchor.pane_id)
            .and_then(|row| row.pane.as_ref())
            .is_some_and(|pane| ui.selected_pane.as_ref() == Some(&pane.pane_id))
            && anchor.issued_at_ms > ui.last_focus_anchor_ms
            && crate::mux::focus_anchor::is_fresh(presentation_at_ms, now_ms)
        {
            self.focus_landing = anchor
                .placement
                .as_ref()
                .map(|_| (anchor.clone(), clock.now()));
            if let Some(placement) = &anchor.placement {
                ui.focus_placement = Some(placement.clone());
                ui.scroll_ease.cancel();
            }
            if let Some(order) = &anchor.order
                && self.live_focus_source != Some(anchor.nonce)
            {
                layout_freeze::adopt_shared_order(
                    ui,
                    &mut self.current,
                    order.clone(),
                    Some(now_ms as i64),
                );
            }
            ui.last_focus_anchor_ms = anchor.issued_at_ms;
        }
        if self.confirmed_focus_intent_ms == anchor.issued_at_ms {
            if self
                .focus_rollback
                .as_ref()
                .is_some_and(|request| request.target == anchor.pane_id)
            {
                self.focus_rollback = None;
            }
            self.confirmed_focus_intent_ms = 0;
            return true;
        }
        false
    }

    pub(super) fn retire_anchor(&mut self, anchor: &FocusAnchor, retired: Instant) {
        self.observed_anchor = Some((retired, None));
        self.retired_anchor = Some((retired, anchor.issued_at_ms));
        if self.confirmed_focus_intent_ms == anchor.issued_at_ms {
            self.confirmed_focus_intent_ms = 0;
        }
    }

    /// Undo this view's optimistic jump to `target`, returning the jump's label; `None` when
    /// this view did not dispatch it.
    pub(super) fn roll_back_focus(
        &mut self,
        state: &mut ViewState,
        target: &PaneId,
        now: Instant,
    ) -> Option<String> {
        let rollback = self
            .focus_rollback
            .take_if(|request| request.target == *target)?;
        state.baseline_pane = rollback.baseline;
        state.browse = None;
        state.focus_placement = None;
        self.focus_landing = None;
        // Dispatch removed the failed intent. Fence older retained cuts from reviving it,
        // but do not fence the pulled baseline that the failed jump must restore.
        self.observed_anchor = Some((now, None));
        self.retired_anchor = None;
        Some(rollback.label)
    }

    pub(super) fn alert_focus_failure(&mut self, label: &str, error: &str, now: Timestamp) {
        if self
            .health
            .alert
            .as_ref()
            .is_some_and(render::Alert::is_active)
        {
            return;
        }
        self.health.alert = Some(render::Alert {
            reason: format!("focus failed: {label}: {error}"),
            since: now,
            recovered_at: Some(now),
        });
    }

    pub(super) fn fuse_pulled(
        &mut self,
        pulled: SidebarSnapshot,
        events: EventStore,
        intent: Option<&FocusPresentation>,
        now_ms: u64,
    ) -> (SidebarSnapshot, Option<u64>) {
        let (snapshot, baseline, sent_at_ms) = fuse_owned(pulled, events, intent, now_ms);
        self.overlay_baseline = baseline;
        (snapshot, sent_at_ms)
    }

    pub(super) fn commit_fetch(
        &mut self,
        update: FetchUpdate,
        ui: &mut ViewState,
        now: Timestamp,
    ) -> (SidebarSnapshot, bool, Vec<DiagEvent>) {
        let (snapshot, source) = match update {
            FetchUpdate::Snapshot {
                snapshot, source, ..
            } => (Ok(*snapshot), Some(source)),
            FetchUpdate::Failed { error } => (Err(error), None),
            FetchUpdate::Shared { .. } => unreachable!("unwrapped before application"),
            FetchUpdate::Fold { .. } => unreachable!("projected at take"),
            FetchUpdate::Unchanged { .. } => {
                unreachable!("clock-only delivery bypasses the content path")
            }
        };
        // The gate compares the incoming snapshot against the last frame we actually
        // committed; `current` still holds it until we overwrite it below.
        let fetch_was_ok = snapshot.is_ok();
        let fetch_failure = snapshot.as_ref().err().cloned();
        let producer_verdict = source == Some(SnapshotSource::Produced);
        let mut computed = compute_next_state(snapshot, &self.current, &self.health, now);
        if fetch_was_ok && !producer_verdict {
            // Published fast folds are paintable data, not a producer-health
            // verdict. Only a completed produce can recover the refresh episode,
            // so frameless/status-only folds cannot mask repeated pane-read failure.
            computed.health = self.health.clone();
        }
        let incoming_panes_produced_at_ms = computed.snapshot.panes_produced_at_ms;
        let (state, next_gate, rejected, released_via_escape_hatch, rejected_snapshot) =
            apply_gate(computed, fetch_was_ok, &self.current, &self.gate, now);
        if self.overlay_baseline.is_none() {
            self.overlay_baseline = rejected_snapshot;
        }
        let diagnostics = fetch_diagnostics(FetchDiagnostics {
            prev_snapshot: &self.current,
            incoming_panes_produced_at_ms,
            next_snapshot: &state.snapshot,
            prev_health: &self.health,
            next_health: &state.health,
            prev_gate: &self.gate,
            next_gate: &next_gate,
            fetch_failure,
            rejected,
            released_via_escape_hatch,
            now,
        });
        self.gate = next_gate;
        self.health = state.health;
        let previous = std::mem::replace(&mut self.current, state.snapshot);
        if fetch_was_ok {
            if let Some(outage) = &self.current.hook_drainer_outage {
                ui.hook_drainer_alert = Some(render::Alert::active(
                    "hook drainer unreachable",
                    outage.since,
                ));
            } else if let Some(alert) = &mut ui.hook_drainer_alert
                && alert.is_active()
            {
                alert.recovered_at = Some(self.current.now);
            }
        }
        if self
            .health
            .alert
            .as_ref()
            .is_some_and(render::Alert::is_active)
        {
            ui.search_draft = None;
        }
        (previous, rejected, diagnostics)
    }

    pub(super) fn read_receipts(
        &mut self,
        ui: &mut ViewState,
        context: &ReadContext<'_>,
    ) -> (Option<PaneId>, ReadClear) {
        let ReadContext {
            own_pane,
            marks,
            now,
            ..
        } = *context;
        let focused_pane = session_focus_baseline(&self.current, own_pane);
        let viewing_register_pane = self
            .current
            .focused_pane
            .as_ref()
            .is_some_and(|pane| self.current.viewed_panes.contains(pane));
        let focused_row_id = focused_pane
            .as_ref()
            .filter(|pane| {
                viewing_register_pane && self.current.focused_pane.as_ref() == Some(*pane)
            })
            .and_then(|pane| row_id_of_pane(&self.current, pane));
        let mut clear = read_receipt_for_row(
            &self.current,
            focused_row_id.as_deref(),
            UnreadClearCause::Focus,
            marks,
            now,
        );
        self.sweep_tab_read_receipts(ui, focused_row_id.as_deref(), context, &mut clear);
        apply_manual_unread_guard(ui, focused_row_id.as_deref(), &mut clear);
        set_rows_unread(&mut self.current, &clear.ids, false);
        (focused_pane, clear)
    }

    pub(super) fn sweep_tab_read_receipts(
        &mut self,
        ui: &mut ViewState,
        focused_row_id: Option<&str>,
        context: &ReadContext<'_>,
        clear: &mut ReadClear,
    ) {
        let ReadContext {
            own_pane,
            marks,
            now,
            at,
        } = *context;
        let Some(view) = self.current.own_view.as_ref() else {
            return;
        };
        let now_viewing = own_tab_viewed(&self.current.viewed_panes, view, own_pane);
        let switched_in = ui.viewing_own_tab == Some(false) && now_viewing;
        ui.viewing_own_tab = Some(now_viewing);
        if !now_viewing {
            // Left the tab: a scan, not a read. Drop the pending sweep.
            self.tab_read_dwell_until = None;
        } else if switched_in {
            self.tab_read_dwell_until = Some(at + TAB_READ_DWELL);
            layout_freeze::arm_freeze(ui, now.as_millisecond());
            // Spend that accrued while hidden is not a climb the user watched:
            // unseeded rolls make this fold's `fold_spend_rolls` snap.
            ui.tally = Default::default();
            ui.cost_rolls = Default::default();
        }
        if now_viewing
            && self.tab_read_dwell_until.is_some_and(|until| at >= until)
            && let Some(view) = self.current.own_view.as_ref()
        {
            self.tab_read_dwell_until = None;
            clear.merge(read_receipts_for_tab(
                &self.current,
                &view.working_pane_ids,
                focused_row_id,
                marks,
                now,
            ));
        }
    }

    pub(super) fn reconcile_selection_and_order(
        &mut self,
        ui: &mut ViewState,
        fold: SelectionFold<'_>,
    ) -> (bool, bool) {
        let SelectionFold {
            previous,
            selected,
            focused,
            cleared,
            filter,
            mux,
            clock,
            now,
            parked,
        } = fold;
        let now_ms = clock.timestamp_at(now).as_millisecond();
        // Presentation sort reorders the snapshot's full row set. The layout freeze
        // below can keep this sorted order stable across a read-clear long enough
        // for the user to confirm where they landed.
        self.current.sort_groups_for_presentation();
        // Reconcile the highlight as part of the fold, before the next frame paints:
        // re-anchor the identity-keyed selection to its row (so a status-churn
        // reorder never slides it onto a neighbour) and re-derive the baseline from
        // the view's agreed working-pane focus. Its pending intent takes priority
        // over the sampled clients. The derivation is filtered to a non-sidebar
        // row: sidebar focus, disagreement or non-row focus derives `None` and the
        // baseline holds its last value. It is
        // deliberately blind to the make-up filter — the focused pane is real
        // however the body is narrowed, so a hidden baseline holds rather than
        // blanks.
        let derived_focus_pane = focused.is_some();
        let authoritative_filter_fold = filter.is_some();
        if let Some(filter) = filter {
            set_make_up_filter(ui, &self.current, filter);
        }
        let previous_filter = ui.make_up_filter.filter;
        reconcile_selection(ui, &self.current, focused);
        if derived_focus_pane && ui.selected_pane.is_some() && ui.selected_pane != selected {
            let dwell = match mux {
                MuxName::Tmux => FOCUS_MARKER_TMUX_DWELL,
                MuxName::Zellij => FOCUS_MARKER_ZELLIJ_DWELL,
            };
            self.focus_marker = Some(FocusMarker::Dwelling(clock.now() + dwell));
        }
        let persist_filter = authoritative_filter_fold
            && !self.current.worktree_groups.is_empty()
            && previous_filter.is_some()
            && ui.make_up_filter.filter.is_none();
        if derived_focus_pane && let Some(requested) = &self.pending_input_focus {
            let landed = self
                .current
                .row_owning_pane(requested)
                .and_then(|row| row.pane.as_ref())
                .is_some_and(|pane| ui.selected_pane.as_ref() == Some(&pane.pane_id));
            if landed {
                ui.own_input_reveal = true;
                self.pending_input_focus = None;
            } else if ui.selected_pane != selected {
                self.pending_input_focus = None;
            }
        }
        let acted =
            layout_freeze::focused_interaction(previous, &self.current, ui.selected_pane.as_ref());
        let interacted = cleared || ui.selected_pane != selected || acted;
        let held = ui.freeze.is_some();
        layout_freeze::apply_freeze(ui, &mut self.current, interacted, now_ms, parked);
        (persist_filter, held && ui.freeze.is_none())
    }

    pub(super) fn fold_spend_rolls(&self, ui: &mut ViewState, clock: &LoopClock) {
        frame_clock(
            clock.now(),
            self.anim_start,
            self.current.theme.display.resolved_refresh_ms(),
        )
        .stamp(ui);
        // Fold the fresh headline spend into the count-up: a higher figure starts a
        // stepped roll that the next frames paint, a reset or first value snaps,
        // and an unchanged one is a no-op that leaves a climb in flight. The live
        // overlay is the preferred target — it moves with every statusline push —
        // falling back to the walked tally on a pre-overlay snapshot. A fetch
        // carrying neither leaves the roll untouched, so a transient missing
        // snapshot never snaps the figure to zero. The serve loop paints the
        // folded state on its next frame boundary; this path never draws.
        if let Some((usd, epoch)) = render::cockpit_spend_target(&self.current) {
            let shown = ui.spend_ratchet.observe(epoch, usd);
            ui.tally.observe(shown, ui.animation_phase);
        }
        // The per-card cost rolls fold beside it: live cards observe session cost,
        // while a finished cohort observes each durable seat's lifetime cost.
        // Both use the same resolved target the renderer paints, keyed by durable
        // row id, so finishing a group rolls each card up to the receipt figures.
        // A row without cost enrichment is not observed; its first cost snaps.
        ui.cost_rolls.observe(
            self.current.worktree_groups.iter().flat_map(|group| {
                group.rows.iter().filter_map(|row| {
                    render::agent_card_cost_usd(group, row).map(|usd| (row.id.clone(), usd))
                })
            }),
            ui.animation_phase,
        );
    }
}
