use std::collections::{HashMap, HashSet};

use crate::ids::PaneId;
use crate::sidebar::frame::{PaneFrame, PaneMetrics};
use crate::sidebar::timing::PROCESS_START_MATCH_TOLERANCE;

fn pane_process_agent_kind(process: &crate::sidebar::frame::PaneProcess) -> Option<&'static str> {
    process
        .spawn_command
        .as_deref()
        .and_then(crate::ledger::snapshot::command_agent_kind)
        .or_else(|| {
            process
                .command
                .as_deref()
                .and_then(crate::ledger::snapshot::command_agent_kind)
        })
}

/// Stamp the in-pane agent CLI's `/proc` start onto agent panes the backend
/// left without one — every pane today: tmux has no per-pane process-start
/// format variable, and Zellij 0.44 emits no process fields (`RawPane` keeps
/// reading builds that do). A startless pane leaves the cwd-fallback guard
/// (`pane_start_allows_bind`) blind, so a stale daemon-mode Codex session would
/// latch onto a freshly-started pane in the same cwd and project its old stats.
/// Runs at frame production, in both produce arms, so the published pane frame
/// carries the stamp and every reader — the in-process produce *and* the
/// consumer in-process fold (`read_published_snapshot`) — sees the guard fire;
/// stamping after the fold's frame read would leave the consumer lane blind
/// again.
///
/// Only panes in `unstamped` — left startless by the fresh read itself
/// ([`natively_unstamped`]) — are touched; a native start is authoritative.
/// For those, the derive ladder is freshest-first:
/// 1. the agent CLI behind the pane's bound root process (`pane_pid`,
///    established and starttime-revalidated by the metrics cadence in
///    [`super::metrics::enrich_pane_metrics`]) — per-pane exact, re-derived
///    each produce, so a re-tenanted pane (the agent exits and is re-run in
///    place) sheds the prior tenant's stamp;
/// 2. the stamp the frame rotation restored from the prior frame — bridges the
///    windows where the binding is missing or its process is gone
///    (a fresh-window re-tenancy, an exited pane) without rescanning;
/// 3. the only unaccounted in-pane agent CLI in the pane's cwd — the warmup
///    path for a pane no prior frame has stamped and no binding has reached
///    yet. Multiple unaccounted starts abstain; duplicating one cwd-level
///    timestamp onto several panes would erase the ordering signal the bind
///    guard needs.
///
/// The derivers are injected for tests; production passes
/// [`crate::remote_control::in_pane_agent_start_for_root`] and
/// [`crate::remote_control::in_pane_agent_starts`].
pub(super) fn stamp_pane_process_starts(
    frame: &mut PaneFrame,
    unstamped: &HashSet<PaneId>,
    root_start: &dyn Fn(&str, u32) -> Option<jiff::Timestamp>,
    cwd_starts: &dyn Fn(&str, &str) -> Vec<jiff::Timestamp>,
) {
    let mut root_stamped = HashSet::new();
    for pane in frame.pane_states_mut() {
        if !unstamped.contains(&pane.pane_id) {
            continue;
        }
        let Some(kind) = pane_process_agent_kind(&pane.current) else {
            continue;
        };
        if let Some(start) = pane.current.pid.and_then(|pid| root_start(kind, pid)) {
            pane.current.started_at = Some(start);
            root_stamped.insert(pane.pane_id.clone());
        }
    }

    clear_duplicate_carried_starts(frame, unstamped, &root_stamped);

    let mut unresolved_by_cwd: HashMap<(String, String), Vec<PaneId>> = HashMap::new();
    let mut accounted_by_cwd: HashMap<(String, String), Vec<jiff::Timestamp>> = HashMap::new();
    for pane in frame.pane_states() {
        let Some(kind) = pane_process_agent_kind(&pane.current) else {
            continue;
        };
        let Some(cwd) = pane.current.cwd.as_deref().filter(|cwd| !cwd.is_empty()) else {
            continue;
        };
        let key = (kind.to_owned(), cwd.to_owned());
        if let Some(start) = pane.current.started_at {
            accounted_by_cwd.entry(key).or_default().push(start);
        } else if unstamped.contains(&pane.pane_id) {
            unresolved_by_cwd
                .entry(key)
                .or_default()
                .push(pane.pane_id.clone());
        }
    }

    let mut exact_assignments: HashMap<PaneId, jiff::Timestamp> = HashMap::new();
    for ((kind, cwd), pane_ids) in unresolved_by_cwd {
        if pane_ids.len() != 1 {
            continue;
        }
        let mut unaccounted = cwd_starts(&kind, &cwd)
            .into_iter()
            .filter(|start| {
                !accounted_by_cwd
                    .get(&(kind.clone(), cwd.clone()))
                    .is_some_and(|accounted| accounted.iter().any(|known| known == start))
            })
            .collect::<Vec<_>>();
        unaccounted.sort();
        unaccounted.dedup();
        if let [start] = unaccounted.as_slice() {
            exact_assignments.insert(pane_ids[0].clone(), *start);
        }
    }

    for pane in frame.pane_states_mut() {
        if let Some(start) = exact_assignments.get(&pane.pane_id) {
            pane.current.started_at = Some(*start);
        }
    }
}

/// Drop a pane's process binding when the live process no longer matches the
/// published start stamp. Agent panes compare against the same root→agent-child
/// derivation [`stamp_pane_process_starts`] uses; a shell-hosted Codex pane's
/// `pid` is the shell while `started_at` is the agent child.
pub(super) fn drop_reused_pid_bindings(
    frame: &mut PaneFrame,
    root_start: &dyn Fn(&str, u32) -> Option<jiff::Timestamp>,
    process_start: &dyn Fn(u32) -> Option<jiff::Timestamp>,
) {
    for pane in frame.pane_states_mut() {
        let Some(pid) = pane.current.pid else {
            continue;
        };
        let Some(expected) = pane.current.started_at else {
            continue;
        };
        let live_start = pane_process_agent_kind(&pane.current)
            .and_then(|kind| root_start(kind, pid))
            .or_else(|| process_start(pid));
        let stale = match live_start {
            Some(actual) => process_start_diff_gt(expected, actual),
            None => true,
        };
        if stale {
            pane.current.pid = None;
            pane.current.started_at = None;
            pane.previous = None;
            pane.children.clear();
            pane.metrics = PaneMetrics::default();
        }
    }
}

fn process_start_diff_gt(left: jiff::Timestamp, right: jiff::Timestamp) -> bool {
    left.as_second().abs_diff(right.as_second()) > PROCESS_START_MATCH_TOLERANCE.as_secs()
}

fn clear_duplicate_carried_starts(
    frame: &mut PaneFrame,
    unstamped: &HashSet<PaneId>,
    root_stamped: &HashSet<PaneId>,
) {
    let mut counts: HashMap<(String, String, jiff::Timestamp), usize> = HashMap::new();
    for pane in frame.pane_states() {
        let Some(start) = pane.current.started_at else {
            continue;
        };
        let Some(kind) = pane_process_agent_kind(&pane.current) else {
            continue;
        };
        let Some(cwd) = pane.current.cwd.as_deref().filter(|cwd| !cwd.is_empty()) else {
            continue;
        };
        *counts
            .entry((kind.to_owned(), cwd.to_owned(), start))
            .or_default() += 1;
    }

    for pane in frame.pane_states_mut() {
        if !unstamped.contains(&pane.pane_id) || root_stamped.contains(&pane.pane_id) {
            continue;
        }
        let Some(start) = pane.current.started_at else {
            continue;
        };
        let Some(kind) = pane_process_agent_kind(&pane.current) else {
            continue;
        };
        let Some(cwd) = pane.current.cwd.as_deref().filter(|cwd| !cwd.is_empty()) else {
            continue;
        };
        if counts
            .get(&(kind.to_owned(), cwd.to_owned(), start))
            .copied()
            .unwrap_or_default()
            > 1
        {
            pane.current.started_at = None;
        }
    }
}
