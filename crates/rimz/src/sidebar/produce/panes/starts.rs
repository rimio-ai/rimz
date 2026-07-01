use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::carry::expired_at;
use crate::ids::{AgentKind, PaneId};
use crate::remote_control::InPaneAgentProcess;
use crate::sidebar::frame::{PaneFrame, PaneMetrics, PaneState};
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
        .or_else(|| {
            process
                .hosted_agent_kind
                .as_ref()
                .and_then(|kind| crate::agents::descriptor_by_kind(kind.as_str()))
                .map(|descriptor| descriptor.kind)
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

/// Stamp the lazy agent CLI process currently hosted under each pane root,
/// independent of the mux-reported foreground command. A live Codex/OpenCode
/// pane can report `git`, `rg`, or a build while the agent process remains the
/// root's child; the row binder consumes this signal to keep the session card
/// stable until the in-pane agent process actually exits.
pub(super) fn stamp_hosted_agent_processes(
    frame: &mut PaneFrame,
    root_process: &dyn Fn(&str, u32) -> Option<InPaneAgentProcess>,
) {
    for pane in frame.pane_states_mut() {
        pane.current.hosted_agent_kind = None;
        pane.current.hosted_agent_process_start = None;
        pane.hosted_carry_since_ms = None;
        let Some(pid) = pane.current.pid else {
            continue;
        };
        let mut hosted = lazy_agent_kinds()
            .filter_map(|kind| root_process(kind, pid).map(|process| (kind, process)))
            .collect::<Vec<_>>();
        hosted.sort_by_key(|(kind, process)| (*kind, process.started_at));
        hosted.dedup_by(|left, right| left.0 == right.0 && left.1.started_at == right.1.started_at);
        if let [(kind, process)] = hosted.as_slice() {
            pane.current.hosted_agent_kind = Some(AgentKind::new_unchecked(*kind));
            pane.current.hosted_agent_process_start = Some(process.started_at);
            if pane.current.cwd.as_deref().is_none_or(|cwd| cwd.is_empty())
                && let Some(cwd) = displayable_cwd(process.cwd.as_ref())
            {
                pane.current.cwd = Some(cwd);
            }
        }
    }
}

/// Restore a hosted lazy-agent stamp across a transient root-process scan miss.
/// The carry is bounded by the same pane-carry TTL and anchored to the first
/// missed scan, so a real exit demotes once the miss stops being transient.
pub(super) fn carry_hosted_agent_stamps(
    frame: &mut PaneFrame,
    prior: Option<&PaneFrame>,
    now_ms: u64,
) {
    let Some(prior) = prior else {
        return;
    };
    let prior_by_pane = prior
        .pane_states()
        .map(|pane| (pane.pane_id.clone(), pane))
        .collect::<HashMap<_, _>>();

    for fresh in frame.pane_states_mut() {
        if fresh.current.hosted_agent_kind.is_some()
            || fresh.current.hosted_agent_process_start.is_some()
        {
            fresh.hosted_carry_since_ms = None;
            continue;
        }
        let Some(prior) = prior_by_pane.get(&fresh.pane_id) else {
            continue;
        };
        let (Some(prior_kind), Some(prior_start)) = (
            prior.current.hosted_agent_kind.as_ref(),
            prior.current.hosted_agent_process_start,
        ) else {
            continue;
        };
        if !pane_state_start_allows_hosted_carry(prior, fresh) {
            continue;
        }
        if pane_process_agent_kind(&fresh.current).is_some_and(|kind| kind != prior_kind.as_str()) {
            continue;
        }
        let carried_since_ms = prior.hosted_carry_since_ms.unwrap_or(now_ms);
        if expired_at(carried_since_ms, now_ms) {
            continue;
        }

        fresh.current.hosted_agent_kind = Some(prior_kind.clone());
        fresh.current.hosted_agent_process_start = Some(prior_start);
        fresh.hosted_carry_since_ms = Some(carried_since_ms);
    }
}

fn pane_state_start_allows_hosted_carry(prior: &PaneState, fresh: &PaneState) -> bool {
    match (prior.current.started_at, fresh.current.started_at) {
        (Some(prior), Some(fresh)) => prior <= fresh,
        _ => true,
    }
}

fn displayable_cwd(cwd: Option<&PathBuf>) -> Option<String> {
    let cwd = cwd?;
    cwd.exists()
        .then(|| cwd.clone())
        .and_then(|cwd| cwd.into_os_string().into_string().ok())
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
        let Some(expected) = pane
            .current
            .hosted_agent_process_start
            .or(pane.current.started_at)
        else {
            continue;
        };
        let hosted_kind = pane
            .current
            .hosted_agent_kind
            .as_ref()
            .map(|kind| kind.as_str());
        let live_start_from_hosted_root = pane_process_agent_kind(&pane.current)
            .or(hosted_kind)
            .and_then(|kind| root_start(kind, pid));
        let missing_carried_hosted_scan = pane.hosted_carry_since_ms.is_some()
            && pane.current.hosted_agent_process_start.is_some()
            && hosted_kind.is_some()
            && live_start_from_hosted_root.is_none();
        let live_start = if missing_carried_hosted_scan {
            None
        } else {
            live_start_from_hosted_root.or_else(|| process_start(pid))
        };
        let stale = match live_start {
            Some(actual) => process_start_diff_gt(expected, actual),
            None if missing_carried_hosted_scan => false,
            None => true,
        };
        if stale {
            pane.current.pid = None;
            pane.current.started_at = None;
            pane.current.hosted_agent_kind = None;
            pane.current.hosted_agent_process_start = None;
            pane.hosted_carry_since_ms = None;
            pane.previous = None;
            pane.children.clear();
            pane.metrics = PaneMetrics::default();
        }
    }
}

fn lazy_agent_kinds() -> impl Iterator<Item = &'static str> {
    crate::agents::known_kinds().filter(|kind| {
        crate::agents::descriptor_by_kind(kind)
            .is_some_and(|descriptor| descriptor.capabilities.registers_lazily)
    })
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
