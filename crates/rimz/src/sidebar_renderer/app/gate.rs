//! The last-known-good commit gate: hold a fetched frame that *succeeded but
//! regressed transiently* — the phantom Agent→Process flicker — so a one-frame
//! binding glitch never reaches the screen, bounded by a count and wall-clock
//! escape hatch so a genuine exit still surfaces promptly.

use std::collections::HashSet;
use std::time::Duration;

use crate::SidebarSnapshot;
use crate::ids::PaneId;
use jiff::Timestamp;

use super::state::RenderState;

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
}

/// Consecutive holds before the escape hatch accepts a regression anyway. Each
/// reject fires one immediate self-heal refetch. The rollup is now read fresh
/// from the atomic `latest.json` each fold (it only ever reflects committed
/// events), so a multi-frame transient agent-drop no longer occurs — the gate
/// needs to absorb only a single slipped frame. Two holds confirm a *genuine*
/// exit (its shell pane survives) and demote it promptly, while a true one-frame
/// flicker recovers on the first reject's refetch and is never accepted.
const ACCEPT_REGRESSION_AFTER_REJECTS: u32 = 2;

/// Hard wall-clock ceiling on a hold episode — the load-bearing hatch, since a
/// slow poll cadence could otherwise stretch the count out. One second caps a
/// genuine exit on the producer tab (whose reject-refetches each pay a
/// `list-panes` round-trip) while staying above a single such round-trip, and
/// well under [`GIVE_UP_AFTER_DEGRADED`](super::health::GIVE_UP_AFTER_DEGRADED).
const ACCEPT_REGRESSION_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitDecision {
    /// Replace the cache with the incoming snapshot.
    Accept,
    /// Keep the prior good snapshot; the incoming one is a transient regression.
    KeepPrior,
}

/// Decide whether `incoming` may replace the last-known-good `prev`. Pure: the
/// clock and the streak arrive as arguments so the escape hatch is
/// deterministic in tests. A regression is held only while the *panel set is
/// unchanged* and a pane that `prev` rendered as an agent (or remote-control)
/// host now renders as a bare process — exactly the phantom-`process` flicker.
/// Persistence, not the rollup's `agents` list, distinguishes a transient drop
/// (recovers next read) from a genuine exit (persists until the hatch opens),
/// because the root-cause race is the agent momentarily *leaving* that list.
fn gate_commit(
    prev: &SidebarSnapshot,
    incoming: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> CommitDecision {
    if pane_id_set(prev) != pane_id_set(incoming) {
        // The room genuinely changed (a pane opened or closed); never hold.
        return CommitDecision::Accept;
    }
    if !demotes_agentish_to_process(prev, incoming) {
        return CommitDecision::Accept;
    }
    if escape_hatch_open(gate, now) {
        return CommitDecision::Accept;
    }
    CommitDecision::KeepPrior
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
/// `incoming` — the Agent→Process demotion the gate protects against.
fn demotes_agentish_to_process(prev: &SidebarSnapshot, incoming: &SidebarSnapshot) -> bool {
    let agentish: HashSet<&PaneId> = prev
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.row_kind == crate::SidebarRowKind::Agent)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect();
    incoming
        .worktree_groups
        .iter()
        .flat_map(|group| &group.rows)
        .filter(|row| row.row_kind == crate::SidebarRowKind::Process)
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .any(|pane_id| agentish.contains(pane_id))
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

/// Overlay the last-known-good gate on a freshly computed [`RenderState`].
///
/// A *failed* fetch already fell back to the prior snapshot inside
/// [`compute_next_state`](super::state::compute_next_state), so it is never
/// gated here. A *successful* fetch that [`gate_commit`] judges a transient
/// regression is held: the prior good frame becomes both the rendered snapshot
/// and the next-tick baseline (`last_snapshot`), so the cache never advances
/// onto bad data and the next comparison is still against the last good frame.
/// Returns the possibly-held state, the next gate state, and whether this fetch
/// was rejected (the loop fires one self-heal refetch on a reject).
pub(super) fn apply_gate(
    mut state: RenderState,
    fetch_was_ok: bool,
    prev_good: &SidebarSnapshot,
    gate: &GateState,
    now: Timestamp,
) -> (RenderState, GateState, bool) {
    if fetch_was_ok
        && gate_commit(prev_good, &state.snapshot, gate, now) == CommitDecision::KeepPrior
    {
        state.snapshot = prev_good.clone();
        state.last_snapshot = Some(prev_good.clone());
        let next = GateState {
            reject_streak: gate.reject_streak.saturating_add(1),
            rejecting_since: gate.rejecting_since.or(Some(now)),
        };
        (state, next, true)
    } else {
        (state, GateState::default(), false)
    }
}

#[cfg(test)]
mod tests;
