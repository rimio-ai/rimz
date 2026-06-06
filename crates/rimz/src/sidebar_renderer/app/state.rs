//! Folding a fetch outcome into what the loop renders: the pure
//! [`compute_next_state`] reducer, the last-known-good gate overlay, the
//! selection reconcile, and the exit verdict.

use std::time::Instant;

use crate::{SidebarSnapshot, WorkspaceId};
use jiff::Timestamp;
use tracing::{debug, warn};

use crate::sidebar_renderer::render::UiState;

use super::fetch::FetchOutcome;
use super::gate::{GateState, apply_gate};
use super::health::{Health, degraded_too_long, next_health};
use super::lifecycle::{SelfCloseState, self_close_decision};
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
        now,
        worktree_groups: Vec::new(),
        needs_attention: Vec::new(),
        resolver_working: Vec::new(),
        agents: Vec::new(),
        agent_hooks_ready: false,
        wired_lazy_kinds: Vec::new(),
        lazy_agent_default_models: std::collections::BTreeMap::new(),
        own_view: None,
        only_daemon_view_remains: false,
        project_root: None,
        worktree_roots: Vec::new(),
        root_class: crate::workspace::RootClass::Repo,
        sidebar: crate::config::SidebarConfig::default(),
        providers: Vec::new(),
        value_tally: None,
        today_spend_live_usd: None,
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

/// What [`apply_fetch_outcome`] reports back to the loop: whether to exit, and
/// whether this fetch was held as a transient regression (the loop fires one
/// self-heal refetch so the cache reaches the next good frame).
pub(super) struct ApplyOutcome {
    pub(super) should_exit: bool,
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
    anim_start: Instant,
) -> Result<ApplyOutcome> {
    // The gate compares the incoming snapshot against the last frame we actually
    // committed; `current` still holds it until we overwrite it below.
    let fetch_was_ok = outcome.snapshot.is_ok();
    let prev_good = current.clone();
    let computed = compute_next_state(
        &config.workspace_id,
        None,
        outcome.snapshot,
        last_snapshot.take(),
        health,
    );
    let (state, next_gate, rejected) =
        apply_gate(computed, fetch_was_ok, &prev_good, gate, Timestamp::now());
    *gate = next_gate;
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
    let derived = current
        .own_view
        .as_ref()
        .filter(|view| !view.own_is_active)
        .and_then(|view| view.active_pane_id.clone())
        .filter(|pane| row_index_of_pane(current, None, pane).is_some());
    reconcile_selection(ui, current, derived);
    ui.animation_phase = wall_clock_phase(anim_start);
    // Fold the fresh today-spend into the count-up: a higher figure starts a
    // stepped roll that the next frames paint, a reset or first value snaps,
    // and an unchanged one is a no-op that leaves a climb in flight. The live
    // overlay is the preferred target — it moves with every statusline push —
    // falling back to the walked tally on a pre-overlay snapshot. A fetch
    // carrying neither leaves the roll untouched, so a transient missing
    // snapshot never snaps the figure to zero. The serve loop paints the
    // folded state on its next frame boundary; this path never draws.
    let today_usd = current
        .today_spend_live_usd
        .or(current.value_tally.as_ref().map(|tally| tally.today.usd));
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
                row.context
                    .as_ref()
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
            rejected,
        });
    }
    Ok(ApplyOutcome {
        should_exit: false,
        rejected,
    })
}

#[cfg(test)]
mod tests;
