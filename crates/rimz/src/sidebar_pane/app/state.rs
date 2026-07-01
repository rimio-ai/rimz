//! Folding a fetch outcome into what the loop renders: the pure
//! [`compute_next_state`] reducer, the last-known-good gate overlay, the
//! selection reconcile, and the exit verdict.

use std::collections::HashSet;
use std::time::Instant;

use crate::{SidebarSnapshot, WorkspaceId};
use jiff::Timestamp;
use tracing::{debug, warn};

use crate::sidebar::read_marks::ReadMarkStore;
use crate::sidebar::unread::{self, ClearedUnread, UnreadClearCause};
use crate::sidebar_pane::render::{GateNotice, UiState};

use super::fetch::FetchOutcome;
use super::gate::{GateState, apply_gate, gate_held_ms};
use super::health::{Health, degraded_too_long, next_health};
use super::lifecycle::{SelfCloseState, self_close_decision};
use super::order_hold;
use super::selection::{reconcile_selection, row_index_of_pane};
use super::{Result, ServeConfig, wall_clock_phase};

/// Decide what to render next given the latest heartbeat + snapshot outcomes.
/// Pure data, no I/O — extracted so the loop's recovery rules are testable.
pub fn compute_next_state(
    workspace_id: &WorkspaceId,
    heartbeat_failure: Option<String>,
    snapshot: std::result::Result<SidebarSnapshot, String>,
    previous_snapshot: Option<SidebarSnapshot>,
    previous_health: &Health,
) -> RenderState {
    let (last_snapshot, snapshot_failure) = match snapshot {
        Ok(snapshot) => (Some(snapshot), None),
        Err(reason) => (previous_snapshot, Some(reason)),
    };

    // A failed snapshot is the headline; a heartbeat-only failure still keeps
    // the fresh snapshot but reports its own reason.
    let failure = snapshot_failure
        .map(|reason| format!("snapshot failed: {reason}"))
        .or_else(|| heartbeat_failure.map(|reason| format!("heartbeat failed: {reason}")));

    let health = next_health(previous_health, failure);

    let snapshot_to_render = last_snapshot
        .clone()
        .unwrap_or_else(|| placeholder_snapshot(workspace_id.clone()));

    RenderState {
        snapshot: snapshot_to_render,
        health,
        last_snapshot,
    }
}

pub(super) fn placeholder_snapshot(workspace_id: WorkspaceId) -> SidebarSnapshot {
    let display_name = workspace_id.as_str().to_owned();
    let now = Timestamp::now();
    SidebarSnapshot {
        workspace_id,
        display_name,
        generated_at: now,
        panes_produced_at_ms: None,
        panes_observed_at_ms: None,
        focus_contested_panes: Vec::new(),
        viewed_panes: Vec::new(),
        presence: None,
        truth_degraded: None,
        now,
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        wired_lazy_kinds: Vec::new(),
        lazy_agent_default_models: std::collections::BTreeMap::new(),
        agent_panes: Vec::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: None,
        worktree_roots: Vec::new(),
        worktree_home: None,
        root_class: crate::workspace::RootClass::Repo,
        sidebar: crate::config::SidebarConfig::default(),
        theme: crate::config::ThemeConfig::default(),
        attention: crate::config::AttentionConfig::default(),
        providers: Vec::new(),
        value_tally: None,
        workspace_value_tally: None,
        today_spend_live_usd: None,
        link: None,
        reflects_log: None,
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub struct RenderState {
    pub snapshot: SidebarSnapshot,
    pub health: Health,
    pub last_snapshot: Option<SidebarSnapshot>,
}

/// What [`apply_fetch_outcome`] reports back to the loop: whether to exit,
/// whether that exit is the tab-empty self-close path, and whether this fetch
/// was held as a transient regression (the loop fires one self-heal refetch so
/// the cache reaches the next good frame).
pub(super) struct ApplyOutcome {
    pub(super) should_exit: bool,
    pub(super) tab_emptied: bool,
    pub(super) rejected: bool,
}

/// Fold one fetch outcome into the render state: gate it against the
/// last-known-good frame, update health, snapshot, and selection, draw the
/// frame, and report whether the loop should exit — give up after sustained
/// degradation, or self-close once the tab has emptied. Shared by the first
/// synchronous frame and every background-fetch result so the recovery rules
/// live in one place.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_fetch_outcome(
    config: &ServeConfig,
    outcome: FetchOutcome,
    last_snapshot: &mut Option<SidebarSnapshot>,
    current: &mut SidebarSnapshot,
    health: &mut Health,
    gate: &mut GateState,
    self_close: &mut SelfCloseState,
    ui: &mut UiState,
    read_marks: &mut ReadMarkStore,
    anim_start: Instant,
    diag: Option<&crate::diag::DiagSink>,
) -> Result<ApplyOutcome> {
    // The gate compares the incoming snapshot against the last frame we actually
    // committed; `current` still holds it until we overwrite it below.
    let fetch_was_ok = outcome.snapshot.is_ok();
    let fetch_failure = outcome.snapshot.as_ref().err().cloned();
    let final_for_request = outcome.final_for_request;
    let prev_good = current.clone();
    let prev_health = health.clone();
    let prev_gate = gate.clone();
    let mut computed = compute_next_state(
        &config.workspace_id,
        None,
        outcome.snapshot,
        last_snapshot.take(),
        health,
    );
    if fetch_was_ok && !final_for_request {
        // A fast-lane frame inside an open fetch cycle is paintable data, not a
        // health verdict. Let the final produce outcome recover or extend the
        // refresh episode so a repeated produce failure is not masked by the
        // frameless/status-only fast fold that precedes it.
        computed.health = health.clone();
    }
    let incoming_snapshot = computed.snapshot.clone();
    let now = Timestamp::now();
    let (state, next_gate, rejected, released_via_escape_hatch) =
        apply_gate(computed, fetch_was_ok, &prev_good, gate, now);
    emit_diagnostics(
        diag,
        &prev_good,
        &incoming_snapshot,
        &state.snapshot,
        &prev_health,
        &state.health,
        &prev_gate,
        &next_gate,
        fetch_failure,
        rejected,
        released_via_escape_hatch,
        now,
    );
    *gate = next_gate;
    ui.gate_notice = gate.rule.map(|rule| GateNotice { rule });
    if let Some(alert) = state
        .health
        .alert
        .as_ref()
        .filter(|alert| alert.is_active())
    {
        warn!(reason = %alert.reason, "sidebar refresh degraded");
    }
    *last_snapshot = state.last_snapshot;
    *health = state.health;
    *current = state.snapshot;
    let prev_selected = ui.selected_pane.clone();
    let contested_existing_baseline = current
        .own_view
        .as_ref()
        .is_some_and(|view| view.focus_contested)
        && ui.baseline_pane.is_some();
    let focused_pane = if contested_existing_baseline {
        None
    } else {
        focused_working_pane(current)
    };
    let viewing_active_pane = current
        .own_view
        .as_ref()
        .is_some_and(|view| view.active_pane_is_viewed);
    let focused_row_id = focused_pane
        .as_ref()
        .filter(|_| viewing_active_pane)
        .and_then(|pane| row_id_of_pane(current, pane));
    let marks = read_marks.load_merged();
    let live: HashSet<String> = current
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .map(|row| row.id.clone())
        .collect();
    let mut clear = read_receipt_for_row(
        current,
        focused_row_id.as_deref(),
        UnreadClearCause::Focus,
        &marks,
        now,
    );
    if let Some(view) = current.own_view.as_ref() {
        let now_viewing = view.active_pane_is_viewed;
        let switched_in = ui.viewing_own_tab == Some(false) && now_viewing;
        ui.viewing_own_tab = Some(now_viewing);
        if switched_in {
            clear.merge(read_receipts_for_tab(
                current,
                &view.working_pane_ids,
                focused_row_id.as_deref(),
                &marks,
                now,
            ));
        }
    }
    apply_manual_unread_guard(ui, focused_row_id.as_deref(), &mut clear);
    read_marks.observe_fold(clear.ids.clone(), now.as_millisecond(), &live);
    set_rows_unread(current, &clear.ids, false);
    if let Some(diag) = diag {
        emit_unread_cleared_trace(diag, &clear.trace);
    }
    // Presentation sort only reorders the producer's already-capped visible set.
    // The order hold below can keep this sorted order stable across a read-clear
    // long enough for the user to confirm where they landed.
    current.sort_groups_for_presentation();
    // Reconcile the highlight as part of the fold, before the next frame paints:
    // re-anchor the identity-keyed selection to its row (so a status-churn
    // reorder never slides it onto a neighbour) and re-derive the baseline from
    // the own view's active pane. Selection is derived state — queried from the
    // mux each fold and same-tab by construction — so an external tab switch or
    // focus move lands on the very next frame. The derivation is filtered to a
    // non-sidebar row: a sidebar-self-active or non-row active pane derives
    // `None` and the baseline holds its last value. It is deliberately blind to
    // the make-up filter — the active pane is real however the body is
    // narrowed, so a hidden baseline holds rather than blanks.
    let derived = focused_pane.filter(|pane| row_index_of_pane(current, None, pane).is_some());
    reconcile_selection(ui, current, derived);
    let interacted = !clear.ids.is_empty() || ui.selected_pane != prev_selected;
    order_hold::apply_order_hold(ui, current, interacted, now.as_millisecond());
    ui.last_order = order_hold::capture_order(current);
    ui.animation_phase = wall_clock_phase(anim_start, current.theme.display.resolved_refresh_ms());
    // Fold the fresh headline spend into the count-up: a higher figure starts a
    // stepped roll that the next frames paint, a reset or first value snaps,
    // and an unchanged one is a no-op that leaves a climb in flight. The live
    // overlay is the preferred target — it moves with every statusline push —
    // falling back to the walked tally on a pre-overlay snapshot. A fetch
    // carrying neither leaves the roll untouched, so a transient missing
    // snapshot never snaps the figure to zero. The serve loop paints the
    // folded state on its next frame boundary; this path never draws.
    let today_usd = current.today_spend_live_usd.or(current
        .workspace_value_tally
        .as_ref()
        .map(|tally| tally.headline.usd));
    if let Some(usd) = today_usd {
        ui.tally.observe(usd, ui.animation_phase);
    }
    // The per-card cost rolls fold beside it: observe each agent row's session
    // cost under its durable row id (pruning rows the snapshot no longer
    // carries), so a card's `$cost` ticks up on the next frames the same way.
    // A row without the cost enrichment is simply not observed; when its first
    // cost lands, the first observation snaps — never a `0 → cost` boot roll.
    ui.cost_rolls.observe(
        current
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .filter_map(|row| {
                row.as_agent()
                    .and_then(|agent| agent.context.as_ref())
                    .and_then(|context| context.cost.as_ref())
                    .and_then(|cost| cost.total_cost_usd)
                    .map(|usd| (row.id.clone(), usd))
            }),
        ui.animation_phase,
    );

    // A renderer degraded this long is non-functional and, with a now-stale
    // heartbeat, unreachable by `rimz reload` — so it gives up rather than
    // lingering as a zombie showing a frozen frame. Exiting closes its
    // `close_on_exit` pane; reload/attach recovery then rebuilds a current
    // sidebar against the live panes.
    if degraded_too_long(health, Timestamp::now()) {
        warn!(
            session = %config.session_name,
            reason = health.alert.as_ref().map(|alert| alert.reason.as_str()),
            "sidebar degraded too long; exiting so the pane closes and reload/attach can rebuild it",
        );
        return Ok(ApplyOutcome {
            should_exit: true,
            tab_emptied: false,
            rejected,
        });
    }

    // Own-view (sibling count) rides in on the snapshot — the producer computes
    // it from the same pane list it already enumerated. Presence publication and
    // the poll backstop feed this latch; resize only decides whether to hold a
    // grown-width paint while the fresh fold is pending.
    if self_close_decision(
        self_close,
        current.own_view.as_ref().map(|view| view.sibling_count),
    ) {
        debug!(
            session = %config.session_name,
            "sidebar tab emptied; exiting so the pane closes itself",
        );
        return Ok(ApplyOutcome {
            should_exit: true,
            tab_emptied: true,
            rejected,
        });
    }
    Ok(ApplyOutcome {
        should_exit: false,
        tab_emptied: false,
        rejected,
    })
}

#[allow(clippy::too_many_arguments)]
fn emit_diagnostics(
    diag: Option<&crate::diag::DiagSink>,
    prev_snapshot: &SidebarSnapshot,
    incoming_snapshot: &SidebarSnapshot,
    next_snapshot: &SidebarSnapshot,
    prev_health: &Health,
    next_health: &Health,
    prev_gate: &GateState,
    next_gate: &GateState,
    fetch_failure: Option<String>,
    rejected: bool,
    released_via_escape_hatch: bool,
    now: Timestamp,
) {
    let Some(diag) = diag else {
        return;
    };
    if let Some(reason) = fetch_failure {
        diag.emit(crate::schema::diag::DiagEvent::FetchFailure {
            reason,
            failure_streak: next_health.failure_streak,
        });
    }
    if rejected && let Some(rule) = next_gate.rule {
        diag.emit(crate::schema::diag::DiagEvent::GateHold {
            rule,
            prev_produced_at_ms: prev_snapshot.panes_produced_at_ms,
            incoming_produced_at_ms: incoming_snapshot.panes_produced_at_ms,
            reject_streak: next_gate.reject_streak,
        });
    } else if next_gate.rule.is_none()
        && let Some(rule) = prev_gate.rule
    {
        diag.emit(crate::schema::diag::DiagEvent::GateRelease {
            rule,
            held_ms: gate_held_ms(prev_gate, now),
            via_escape_hatch: released_via_escape_hatch,
        });
    }
    match (prev_health.alert.as_ref(), next_health.alert.as_ref()) {
        (prev, Some(next)) if next.is_active() && !prev.is_some_and(|alert| alert.is_active()) => {
            diag.emit_unlimited(crate::schema::diag::DiagEvent::HealthAlert {
                reason: next.reason.clone(),
                since_ms: next.since.as_millisecond().max(0) as u64,
                recovered_after_ms: None,
            });
        }
        (Some(prev), Some(next)) if prev.is_active() && !next.is_active() => {
            let recovered_after_ms = next.recovered_at.and_then(|recovered| {
                recovered
                    .duration_since(next.since)
                    .as_millis()
                    .try_into()
                    .ok()
            });
            diag.emit_unlimited(crate::schema::diag::DiagEvent::HealthAlert {
                reason: next.reason.clone(),
                since_ms: next.since.as_millisecond().max(0) as u64,
                recovered_after_ms,
            });
        }
        _ => {}
    }
    for event in crate::diag::diff_group_migrations(prev_snapshot, next_snapshot) {
        diag.emit(event);
    }
}

fn focused_working_pane(snapshot: &SidebarSnapshot) -> Option<crate::ids::PaneId> {
    snapshot
        .own_view
        .as_ref()
        .filter(|view| !view.own_is_active)
        .and_then(|view| view.active_pane_id.clone())
        .filter(|pane| row_index_of_pane(snapshot, None, pane).is_some())
}

fn apply_manual_unread_guard(
    ui: &mut UiState,
    focused_row_id: Option<&str>,
    focus_clear: &mut ReadClear,
) {
    let Some(guarded) = ui.unread_guard.clone() else {
        return;
    };
    if focused_row_id == Some(guarded.as_str()) {
        focus_clear.ids.retain(|id| id != &guarded);
        focus_clear.trace.retain(|change| change.row_id != guarded);
    } else {
        ui.unread_guard = None;
    }
}

fn row_id_of_pane(snapshot: &SidebarSnapshot, pane_id: &crate::ids::PaneId) -> Option<String> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find(|row| {
            row.pane
                .as_ref()
                .is_some_and(|pane| pane.pane_id == *pane_id)
        })
        .map(|row| row.id.clone())
}

#[derive(Clone, Debug, Default)]
pub(super) struct ReadClear {
    pub(super) ids: Vec<String>,
    pub(super) trace: Vec<ClearedUnread>,
}

impl ReadClear {
    fn merge(&mut self, other: Self) {
        for id in other.ids {
            if !self.ids.iter().any(|seen| seen == &id) {
                self.ids.push(id);
            }
        }
        self.trace.extend(other.trace);
    }
}

/// The read receipt to write for a row that was just read by one of the
/// renderer/CLI clear paths. Returns the row id to clear and, when the row was
/// unread, the clear trace. An empty result means the row is already read or
/// never needed a look, so nothing is written.
pub(super) fn read_receipt_for_row(
    snapshot: &SidebarSnapshot,
    row_id: Option<&str>,
    cause: UnreadClearCause,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let Some(row_id) = row_id else {
        return ReadClear::default();
    };
    let Some(row) = snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
        .find(|row| row.id == row_id)
    else {
        return ReadClear::default();
    };
    read_receipt_for_row_ref(row, cause, marks, now)
}

fn read_receipt_for_row_ref(
    row: &crate::SidebarRow,
    cause: UnreadClearCause,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let needs_look = row
        .status()
        .is_some_and(crate::agents::AgentStatus::needs_a_look);
    if !row.unread && (!needs_look || unread::receipt_reaches(marks, &row.id, row.last_activity)) {
        return ReadClear::default();
    }
    let trace = row
        .unread
        .then(|| unread::cleared_unread(row, cause, Some(now.as_millisecond())))
        .into_iter()
        .collect();
    ReadClear {
        ids: vec![row.id.clone()],
        trace,
    }
}

/// Read receipts for unread rows whose pane shares the renderer's own tab,
/// excluding the focused row because the focus path already handled it.
fn read_receipts_for_tab(
    snapshot: &SidebarSnapshot,
    working_pane_ids: &[crate::ids::PaneId],
    focused_row_id: Option<&str>,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let mut clear = ReadClear::default();
    for row in snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| group.rows.iter())
    {
        if Some(row.id.as_str()) == focused_row_id {
            continue;
        }
        let in_tab = row
            .pane
            .as_ref()
            .is_some_and(|pane| working_pane_ids.contains(&pane.pane_id));
        if in_tab {
            clear.merge(read_receipt_for_row_ref(
                row,
                UnreadClearCause::TabView,
                marks,
                now,
            ));
        }
    }
    clear
}

/// Set the `unread` bit on the named rows in place — the instant local feedback
/// for a focus/mark-read clear (`false`) or a mark-unread re-flag (`true`),
/// ahead of the durable write the next produce re-derives.
pub(super) fn set_rows_unread(snapshot: &mut SidebarSnapshot, ids: &[String], unread: bool) {
    if ids.is_empty() {
        return;
    }
    for row in snapshot
        .worktree_groups
        .iter_mut()
        .flat_map(|group| group.rows.iter_mut())
    {
        if ids.iter().any(|id| id == &row.id) {
            row.unread = unread;
        }
    }
}

pub(super) fn emit_unread_cleared_trace(diag: &crate::diag::DiagSink, changes: &[ClearedUnread]) {
    use crate::schema::notify_trace::NotifyTraceEvent;
    for change in changes {
        let event = NotifyTraceEvent::UnreadCleared {
            row_id: change.row_id.clone(),
            label: change.label.clone(),
            agent_kind: change.agent_kind.clone(),
            agent_id: change.agent_id.clone(),
            worktree: change.worktree.clone(),
            pane_id: change.pane_id.clone(),
            cause: change.cause.as_str().to_owned(),
            cleared_at_ms: change.cleared_at_ms,
        };
        diag.trace_notify(event);
    }
}

pub(super) fn emit_unread_marked_trace(
    diag: &crate::diag::DiagSink,
    opened: &[unread::OpenedUnread],
) {
    use crate::schema::notify_trace::NotifyTraceEvent;
    for item in opened {
        diag.trace_notify(NotifyTraceEvent::UnreadMarked {
            row_id: item.row_id.clone(),
            label: Some(item.label.clone()),
            agent_kind: Some(item.agent_kind.clone()),
            agent_id: Some(item.agent_id.clone()),
            worktree: item.worktree.clone(),
            pane_id: item.pane_id.clone(),
            status: item.status.as_str().to_owned(),
            episode_ms: item.episode_ms,
        });
    }
}

#[cfg(test)]
mod tests;
