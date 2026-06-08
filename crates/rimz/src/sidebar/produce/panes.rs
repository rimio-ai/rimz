//! The live pane frame: the single-flight `list-panes` cache, the raced-read
//! process rotation, and the `/proc` process-start stamp — everything the
//! producer publishes to `snapshot.json` for consumers to fold in process.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Result;
use crate::ids::{AgentSessionId, MuxName, PaneId};
use crate::ledger::atomic;
use crate::ledger::single_flight::{self, Coalesced};
use crate::mux::PaneListOptions;
use crate::sidebar::cache::{
    effective_pane_ttl, presence_stamp_age_ms, read_snapshot_cache, snapshot_cache_is_fresh,
    unix_now_ms,
};
use crate::sidebar::frame::{PaneFrame, assemble_frame};

/// How a non-producing sidebar waits for the single producer's cache write
/// before giving up and producing locally. ~200ms total (10 × 20ms).
const SNAPSHOT_CACHE_WAIT_STEP: Duration = Duration::from_millis(20);
const SNAPSHOT_CACHE_WAIT_STEPS: u32 = 10;

/// Return a same-session cache entry younger than `ttl`, or `None` when it is
/// absent, stale, for another session, or unreadable. The caller picks the TTL
/// once per produce ([`effective_pane_ttl`]) — `SNAPSHOT_CACHE_TTL` in poll
/// mode, the stretched event-mode TTL while the presence stamp is fresh — and
/// the freshness verdict itself is the library's
/// ([`snapshot_cache_is_fresh`]), so the forced-freshness floor keeps
/// overriding in both modes.
fn fresh_snapshot_cache(
    cache_path: &Path,
    session: &str,
    min_produced_at_ms: Option<u64>,
    ttl: Duration,
) -> Option<PaneFrame> {
    let cache = read_snapshot_cache(cache_path, session)?;
    snapshot_cache_is_fresh(&cache, unix_now_ms(), min_produced_at_ms, ttl).then_some(cache)
}

/// The session's live panes from the mux — the `list-panes` round-trip the
/// snapshot cache amortizes across the fleet. The ledger rollup is read
/// separately (fresh from `latest.json`), so this enumerates only the pane set.
/// One round-trip is the whole cost: the per-view `is_focused` mark rides the
/// pane list itself, so the sidebar's selection baseline needs no second
/// per-client probe.
fn list_session_panes(
    mux: MuxName,
    session: &str,
    workspace_id: crate::WorkspaceId,
    min_topology_produced_at_ms: Option<u64>,
    command_timeout: Option<Duration>,
) -> Result<Vec<crate::feed::PaneRef>> {
    Ok(crate::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
        workspace_id: Some(workspace_id),
        min_topology_produced_at_ms,
        command_timeout,
    })?)
}

/// Join a fresh frame to the last published same-session frame. Raced-null
/// fields repair only when the process identity stayed stable; a command or
/// root-pid change rotates the prior current process to `previous` and keeps
/// the fresh process record clean.
fn rotate_from_cache(frame: &mut PaneFrame, cache_path: &Path, session: &str) {
    if let Some(prev) = read_snapshot_cache(cache_path, session) {
        frame.rotate_against_prior(&prev);
    }
}

/// The pane ids a fresh `list-panes` read left without a process start — the
/// set the `/proc` stamp owns ([`stamp_pane_process_starts`]). Captured before
/// the frame rotates against the prior publish, so a backend-reported start is
/// never confused with Rimz's own derived stamp and never overwritten by one.
fn natively_unstamped(frame: &PaneFrame) -> HashSet<PaneId> {
    frame
        .pane_states()
        .filter(|pane| pane.current.started_at.is_none())
        .map(|pane| pane.pane_id.clone())
        .collect()
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
fn stamp_pane_process_starts(
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
        let Some(kind) = pane
            .current
            .spawn_command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
            .or_else(|| {
                pane.current
                    .command
                    .as_deref()
                    .and_then(crate::ledger::snapshot::command_agent_kind)
            })
        else {
            continue;
        };
        if let Some(start) = pane.current.pid.and_then(|pid| root_start(kind, pid)) {
            pane.current.started_at = Some(start);
            root_stamped.insert(pane.pane_id.clone());
            continue;
        }
        if pane.current.started_at.is_some() {
            continue;
        }
    }

    clear_duplicate_carried_starts(frame, unstamped, &root_stamped);

    let mut unresolved_by_cwd: HashMap<(String, String), Vec<PaneId>> = HashMap::new();
    let mut accounted_by_cwd: HashMap<(String, String), Vec<jiff::Timestamp>> = HashMap::new();
    for pane in frame.pane_states() {
        let Some(kind) = pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
        else {
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
        let Some(kind) = pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
        else {
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
        let Some(kind) = pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
        else {
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

fn stamp_pane_resumed_session_ids(
    frame: &mut PaneFrame,
    root_resume: &dyn Fn(u32) -> Option<AgentSessionId>,
) {
    for pane in frame.pane_states_mut() {
        if pane.current.resumed_session_id.is_some() {
            continue;
        }
        if pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
            != Some("codex")
        {
            continue;
        }
        if let Some(resumed) = pane.current.pid.and_then(root_resume) {
            pane.current.resumed_session_id = Some(resumed);
        }
    }
}

fn repair_pane_frame(
    frame: &mut PaneFrame,
    runtime: &crate::RuntimePaths,
    cache_path: &Path,
    session: &str,
    enrich_metrics: bool,
) {
    let unstamped = natively_unstamped(frame);
    rotate_from_cache(frame, cache_path, session);
    if enrich_metrics {
        super::metrics::enrich_pane_metrics(frame, session, runtime);
    } else {
        super::metrics::backfill_zellij_pane_pids_from_proc(frame, session);
    }
    backfill_pane_cwds(frame, &|pid| crate::proc::cwd(pid));
    stamp_pane_resumed_session_ids(
        frame,
        &crate::remote_control::codex_resumed_session_id_for_root,
    );
    stamp_pane_process_starts(
        frame,
        &unstamped,
        &crate::remote_control::in_pane_agent_start_for_root,
        &crate::remote_control::in_pane_agent_starts,
    );
}

pub fn repaired_pane_frame_for_binding(
    runtime: &crate::RuntimePaths,
    mux: MuxName,
    session: &str,
    command_timeout: Duration,
) -> Result<PaneFrame> {
    let cache_path = runtime.root.join("snapshot.json");
    let panes = match super::pane_list_fixture()? {
        Some(fixture) => fixture,
        None => list_session_panes(
            mux,
            session,
            runtime.workspace_id.clone(),
            None,
            Some(command_timeout),
        )?,
    };
    let mut frame = assemble_frame(panes, unix_now_ms(), session.to_owned());
    repair_pane_frame(&mut frame, runtime, &cache_path, session, false);
    Ok(frame)
}

/// Fill a pane's raced-empty cwd from `/proc/<pane_pid>/cwd` once the root pid
/// is known. A fresh `list-panes` can answer a just-born pane with an empty cwd
/// for a tick; without one the pane groups under `external` and flickers there
/// until the mux reports the path. Only an empty cwd is ever filled — a
/// mux-reported cwd is authoritative because it tracks OSC7/foreground chdir,
/// which can diverge from the root's `/proc` cwd. A `/proc` cwd that no longer
/// exists is also skipped, since Linux annotates deleted cwd targets with a
/// publish-unsafe `" (deleted)"` suffix.
fn backfill_pane_cwds(frame: &mut PaneFrame, proc_cwd: &dyn Fn(u32) -> Option<PathBuf>) {
    for pane in frame.pane_states_mut() {
        if pane
            .current
            .cwd
            .as_deref()
            .is_some_and(|cwd| !cwd.is_empty())
        {
            continue;
        }
        let Some(pid) = pane.current.pid else {
            continue;
        };
        if let Some(cwd) = proc_cwd(pid)
            .filter(|path| path.exists())
            .and_then(|path| path.into_os_string().into_string().ok())
        {
            pane.current.cwd = Some(cwd);
        }
    }
}

/// Return the live pane frame for `session` — the pane list plus the
/// `produced_at_ms` read stamp the renderer's jump guard orders against —
/// sharing one `list-panes` round-trip across every sidebar via a short-lived
/// single-flight cache.
///
/// Fast path: a fresh same-session cache is read back with no mux work. Slow
/// path: a non-blocking `try_lock` elects one producer; losers poll briefly for
/// its write, then fall back to producing locally so a wedged producer never
/// strands them.
pub(super) fn cached_panes_or_produce(
    runtime: &crate::RuntimePaths,
    mux: MuxName,
    session: &str,
    min_pane_cache_ms: Option<u64>,
) -> Result<PaneFrame> {
    let cache_path = runtime.root.join("snapshot.json");

    // Select the pane TTL once per call from the presence stamp: event mode
    // (EVENT_PANE_TTL) while the Zellij push channel is alive, else poll-mode
    // SNAPSHOT_CACHE_TTL. One small stamp read per produce; the fast path, the
    // single-flight `fresh` closure, and the loser re-check all read this one
    // Duration, so a loser never produces what the winner skipped. tmux never
    // writes the stamp, so tmux is always poll mode by construction.
    let pane_ttl = effective_pane_ttl(presence_stamp_age_ms(runtime));

    // One single-flight lock covers both arms: the slow path's full produce
    // and the fast path's metrics-only refresh, so only one elected producer
    // ever writes the shared caches.
    let lock_path = runtime.root.join("snapshot.lock");

    // Fast path: a fresh same-session entry needs no mux work. Metrics still
    // have their own cadence, so refresh them from the cached topology when
    // due instead of waiting for the pane cache to expire.
    if let Some(cache) = fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms, pane_ttl) {
        return Ok(refresh_cached_metrics(
            cache,
            runtime,
            &cache_path,
            &lock_path,
            session,
            min_pane_cache_ms,
            pane_ttl,
        ));
    }

    // Slow path: elect one producer for this `(workspace, session)` refresh.
    // Losers read its write back; if it wedges, they fall back to an uncached
    // local produce rather than block.
    let fresh = || fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms, pane_ttl);
    let produce_local = || -> Result<PaneFrame> {
        Ok(assemble_frame(
            list_session_panes(
                mux,
                session,
                runtime.workspace_id.clone(),
                min_pane_cache_ms,
                None,
            )?,
            unix_now_ms(),
            session.to_owned(),
        ))
    };
    match single_flight::coalesce(
        &lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(cache) => Ok(cache),
        // The producer wedged past the wait: produce locally rather than block.
        // The raced-read repair still applies — without it a dropped command/cwd
        // on this one path folds the anonymous row the winner path guards against.
        Coalesced::ProduceLocal => {
            let mut frame = produce_local()?;
            repair_pane_frame(&mut frame, runtime, &cache_path, session, false);
            Ok(frame)
        }
        // We won: fork `list-panes` and publish it. The guard holds the lock
        // until this arm returns.
        Coalesced::Produce(_guard) => {
            let mut frame = produce_local()?;
            // A mid-tick `list-panes` race can drop a live pane's command/cwd/
            // process-start; rather than fold an anonymous `external`/`process`
            // row that blinks out next tick, run the shared repaired-frame
            // ladder before publishing.
            repair_pane_frame(&mut frame, runtime, &cache_path, session, true);
            publish_frame(runtime, &cache_path, &frame);
            Ok(frame)
        }
    }
}

/// The fast path's metrics arm: re-sample `/proc` over a topology-fresh cached
/// frame when some pane's sample is due, and republish. The publish keeps the
/// frame's `produced_at_ms`, so a metrics-only refresh never masquerades as a
/// fresh pane listing; election rides the same snapshot lock as the full
/// produce, so one process samples per window and a loser serves the shared
/// write back.
fn refresh_cached_metrics(
    frame: PaneFrame,
    runtime: &crate::RuntimePaths,
    cache_path: &Path,
    lock_path: &Path,
    session: &str,
    min_pane_cache_ms: Option<u64>,
    pane_ttl: Duration,
) -> PaneFrame {
    if !super::metrics::pane_metrics_due(&frame, runtime) {
        return frame;
    }
    let fresh = || {
        let cache = fresh_snapshot_cache(cache_path, session, min_pane_cache_ms, pane_ttl)?;
        (!super::metrics::pane_metrics_due(&cache, runtime)).then_some(cache)
    };
    match single_flight::coalesce(
        lock_path,
        SNAPSHOT_CACHE_WAIT_STEP,
        SNAPSHOT_CACHE_WAIT_STEPS,
        fresh,
    ) {
        Coalesced::Shared(cache) => cache,
        // A wedged producer must not block the visible tab. Keep rendering the
        // cached frame rather than writing shared metrics state outside the
        // elected producer path.
        Coalesced::ProduceLocal => frame,
        Coalesced::Produce(_guard) => {
            let mut latest = fresh_snapshot_cache(cache_path, session, min_pane_cache_ms, pane_ttl)
                .unwrap_or(frame);
            if super::metrics::enrich_pane_metrics(&mut latest, session, runtime) {
                publish_frame(runtime, cache_path, &latest);
            }
            latest
        }
    }
}

fn publish_frame(runtime: &crate::RuntimePaths, cache_path: &Path, frame: &PaneFrame) {
    if let Err(err) = atomic::write_temp_then_rename_cache(cache_path, frame) {
        tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
    } else if let Err(err) = crate::ledger::wakeup::wake_sidebars_pane_frame_published(runtime) {
        tracing::debug!(error = %err, "sidebar pane-frame publication wakeup failed");
    }
}

#[cfg(test)]
mod tests;
