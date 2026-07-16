//! Renderer-side last-resort commit gate.
//!
//! Frame plausibility belongs in the producer and row identity belongs in the
//! projection. This gate only absorbs residual mixed-version or rollup races
//! after a fetch succeeded: frameless fallback over a frame-backed render, an
//! old producer's empty stamped frame over populated rows, and the transient
//! Agent→Process demotion.

use crate::SidebarSnapshot;
use crate::diag::record::GateRule;
use crate::ids::PaneId;
use jiff::Timestamp;
use std::collections::{HashMap, HashSet};

use super::state::RenderState;
use crate::sidebar::timing::{ACCEPT_REGRESSION_AFTER, ACCEPT_REGRESSION_AFTER_REJECTS};

/// Sticky state for the last-known-good commit gate, kept beside
/// [`Health`](super::health::Health) but deliberately orthogonal to it:
/// `Health` tracks a *failed fetch*, this tracks a fetch that *succeeded but
/// regressed transiently* and was held. `Gate` never feeds
/// `failure_streak`/`degraded_too_long`, so a sub-second binding glitch
/// neither flashes the degraded banner nor counts toward self-close.
/// `reject_streak` and `rejecting_since` bound how long a regression may be held
/// before the escape hatch releases it (see [`escape_hatch_open`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GateState {
    pub reject_streak: u32,
    pub rejecting_since: Option<Timestamp>,
    pub spend_carry_since: Option<Timestamp>,
    pub rule: Option<GateRule>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitDecision {
    /// Replace the cache with the incoming snapshot.
    Accept,
    /// Replace the cache because a held regression reached the release ceiling.
    AcceptViaEscapeHatch,
    /// Keep the prior good snapshot; the incoming one is a transient regression.
    KeepPrior(GateRule),
}

/// Decide whether `incoming` may replace the last-known-good `prev`. Pure: the
/// clock and the streak arrive as arguments so the escape hatch is
/// deterministic in tests. A regression is held only while the *panel set is
/// unchanged* and a pane that `prev` rendered as an agent (or remote-control)
/// host now renders as a bare process without a positive foreground-command
/// change — exactly the phantom-`process` flicker — or while a frameless
/// fallback tries to replace a frame-backed render. A foreground-command change
/// is a genuine in-place exit and commits immediately.
/// Persistence, not the rollup's `agents` list, distinguishes a transient drop
/// (recovers next read) from a genuine exit (persists until the hatch opens),
/// because the root-cause race is the agent momentarily *leaving* that list.
fn gate_commit(
    prev: &SidebarSnapshot,
    incoming: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> CommitDecision {
    if prev.panes_produced_at_ms.is_some() && incoming.panes_produced_at_ms.is_none() {
        return CommitDecision::KeepPrior(GateRule::FramelessOverFrame);
    }
    if prev.panes_produced_at_ms.is_some()
        && incoming.panes_produced_at_ms.is_some()
        && !pane_id_set(prev).is_empty()
        && pane_id_set(incoming).is_empty()
    {
        if escape_hatch_open(gate, now) {
            return CommitDecision::AcceptViaEscapeHatch;
        }
        return CommitDecision::KeepPrior(GateRule::EmptyStampedFrame);
    }
    if pane_id_set(prev) != pane_id_set(incoming) {
        // The room genuinely changed (a pane opened or closed); never hold.
        return CommitDecision::Accept;
    }
    if !demotes_agentish_to_process(prev, incoming) {
        return CommitDecision::Accept;
    }
    if escape_hatch_open(gate, now) {
        return CommitDecision::AcceptViaEscapeHatch;
    }
    CommitDecision::KeepPrior(GateRule::AgentDemotedToProcess)
}

/// The set of live pane ids a snapshot renders a row for.
fn pane_id_set(snapshot: &SidebarSnapshot) -> HashSet<&PaneId> {
    snapshot
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect()
}

/// True when some pane that `prev` rendered as an agent is a bare process row in
/// `incoming` without a positive foreground-command change — the
/// phantom-`process` flicker the gate protects against.
fn demotes_agentish_to_process(prev: &SidebarSnapshot, incoming: &SidebarSnapshot) -> bool {
    let agentish: HashMap<&PaneId, Option<&str>> = prev
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.is_agent())
        .filter_map(|row| {
            row.pane
                .as_ref()
                .map(|pane| (&pane.pane_id, pane.command.as_deref()))
        })
        .collect();
    incoming
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.is_process())
        .filter_map(|row| row.pane.as_ref())
        .any(|pane| {
            agentish.get(&pane.pane_id).is_some_and(|prev_command| {
                !foreground_command_changed(*prev_command, pane.command.as_deref())
            })
        })
}

fn foreground_command_changed(prev: Option<&str>, incoming: Option<&str>) -> bool {
    matches!((prev, incoming), (Some(prev), Some(incoming)) if prev != incoming)
}

/// Whether a hold episode has run long enough — by count or wall-clock — to
/// accept the regression and stop holding. Mirrors
/// [`degraded_too_long`](super::health::degraded_too_long)'s "never freeze
/// forever" rule for the gate.
fn escape_hatch_open(gate: &GateState, now: Timestamp) -> bool {
    gate.reject_streak >= ACCEPT_REGRESSION_AFTER_REJECTS
        || gate.rejecting_since.is_some_and(|since| {
            now.duration_since(since).as_secs() >= ACCEPT_REGRESSION_AFTER.as_secs() as i64
        })
}

pub(super) fn gate_held_ms(gate: &GateState, now: Timestamp) -> u64 {
    gate.rejecting_since
        .and_then(|since| now.duration_since(since).as_millis().try_into().ok())
        .unwrap_or(0)
}

/// Overlay the last-known-good gate on a freshly computed [`RenderState`].
///
/// A *failed* fetch already fell back to the prior snapshot inside
/// [`compute_next_state`](super::state::compute_next_state), so it is never
/// gated here. A *successful* fetch that [`gate_commit`] judges a transient
/// regression is held: the prior committed frame remains rendered, so the cache
/// never advances onto bad data and the next comparison still uses that frame.
/// Returns the possibly-held state, the next gate state, whether this fetch was
/// rejected (the loop fires one self-heal refetch on a reject), and whether a
/// held regression was accepted by the escape hatch.
pub(super) fn apply_gate(
    mut state: RenderState,
    fetch_was_ok: bool,
    prev_good: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> (RenderState, GateState, bool, bool) {
    if !fetch_was_ok {
        return (state, gate.clone(), false, false);
    }
    match gate_commit(prev_good, &state.snapshot, gate, now) {
        CommitDecision::KeepPrior(rule) => {
            state.snapshot = prev_good.clone();
            let next = GateState {
                reject_streak: gate.reject_streak.saturating_add(1),
                rejecting_since: gate.rejecting_since.or(Some(now)),
                spend_carry_since: gate.spend_carry_since,
                rule: Some(rule),
            };
            (state, next, true, false)
        }
        CommitDecision::AcceptViaEscapeHatch => {
            let spend_carry_since =
                repair_collapsed_spend(prev_good, &mut state.snapshot, gate, now);
            (
                state,
                GateState {
                    spend_carry_since,
                    ..GateState::default()
                },
                false,
                true,
            )
        }
        CommitDecision::Accept => {
            let spend_carry_since =
                repair_collapsed_spend(prev_good, &mut state.snapshot, gate, now);
            (
                state,
                GateState {
                    spend_carry_since,
                    ..GateState::default()
                },
                false,
                false,
            )
        }
    }
}

fn repair_collapsed_spend(
    prev_good: &SidebarSnapshot,
    incoming: &mut SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> Option<Timestamp> {
    if has_nonzero_tally(&incoming.value_tally) {
        return None;
    }
    if !has_nonzero_tally(&prev_good.value_tally) {
        return None;
    }
    let since = gate.spend_carry_since.unwrap_or(now);
    if now.duration_since(since).as_secs() >= ACCEPT_REGRESSION_AFTER.as_secs() as i64 {
        return None;
    }
    incoming.value_tally.clone_from(&prev_good.value_tally);
    incoming
        .workspace_value_tally
        .clone_from(&prev_good.workspace_value_tally);
    incoming
        .today_spend_live_usd
        .clone_from(&prev_good.today_spend_live_usd);
    incoming
        .today_spend_epoch_secs
        .clone_from(&prev_good.today_spend_epoch_secs);
    let prior_spending = prev_good
        .providers
        .iter()
        .filter_map(|panel| {
            panel
                .spending
                .as_ref()
                .map(|spending| (&panel.kind, spending))
        })
        .collect::<HashMap<_, _>>();
    for panel in &mut incoming.providers {
        if let Some(spending) = prior_spending.get(&panel.kind) {
            panel.spending = Some((*spending).clone());
        }
    }
    Some(since)
}

fn has_nonzero_tally(tally: &Option<crate::SpendTally>) -> bool {
    tally.as_ref().is_some_and(|tally| !tally.is_zero())
}

#[cfg(test)]
mod tests;
