//! The live pane frame: the single-flight `list-panes` cache, the raced-read
//! process rotation, and the `/proc` process-start stamp — everything the
//! producer publishes to `snapshot.json` for consumers to fold in process.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::Result;
use crate::ids::{MuxName, PaneId};
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
fn list_session_panes(mux: MuxName, session: &str) -> Result<Vec<crate::feed::PaneRef>> {
    Ok(crate::mux::backend_for(mux).list_panes(PaneListOptions {
        session_name: Some(session.to_owned()),
        ..Default::default()
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
/// 3. the earliest in-pane agent CLI in the pane's cwd — the warmup path for a
///    pane no prior frame has stamped and no binding has reached yet.
///
/// The derivers are injected for tests; production passes
/// [`crate::remote_control::in_pane_agent_start_for_root`] and
/// [`crate::remote_control::in_pane_agent_start`].
fn stamp_pane_process_starts(
    frame: &mut PaneFrame,
    unstamped: &HashSet<PaneId>,
    root_start: &dyn Fn(&str, u32) -> Option<jiff::Timestamp>,
    cwd_start: &dyn Fn(&str, &str) -> Option<jiff::Timestamp>,
) {
    for pane in frame.pane_states_mut() {
        if !unstamped.contains(&pane.pane_id) {
            continue;
        }
        let Some(kind) = pane
            .current
            .command
            .as_deref()
            .and_then(crate::ledger::snapshot::command_agent_kind)
        else {
            continue;
        };
        if let Some(start) = pane.current.pid.and_then(|pid| root_start(kind, pid)) {
            pane.current.started_at = Some(start);
            continue;
        }
        if pane.current.started_at.is_some() {
            continue;
        }
        if let Some(cwd) = pane.current.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
            pane.current.started_at = cwd_start(kind, cwd);
        }
    }
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

    // Fast path: a fresh same-session entry needs no mux work.
    if let Some(cache) = fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms, pane_ttl) {
        return Ok(cache);
    }

    // Slow path: elect one producer for this `(workspace, session)` refresh.
    // Losers read its write back; if it wedges, they fall back to an uncached
    // local produce rather than block.
    let lock_path = runtime.root.join("snapshot.lock");
    let fresh = || fresh_snapshot_cache(&cache_path, session, min_pane_cache_ms, pane_ttl);
    let produce_local = || -> Result<PaneFrame> {
        Ok(assemble_frame(
            list_session_panes(mux, session)?,
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
            let unstamped = natively_unstamped(&frame);
            rotate_from_cache(&mut frame, &cache_path, session);
            // Repair a raced-empty cwd from the pane root before this local
            // frame folds, covering tmux's native pane pid path.
            backfill_pane_cwds(&mut frame, &|pid| crate::proc::cwd(pid));
            // This unpublished fallback frame feeds its own fold directly, so it
            // needs the stamp the cwd-fallback guard reads just like a published one.
            stamp_pane_process_starts(
                &mut frame,
                &unstamped,
                &crate::remote_control::in_pane_agent_start_for_root,
                &crate::remote_control::in_pane_agent_start,
            );
            Ok(frame)
        }
        // We won: fork `list-panes` and publish it. The guard holds the lock
        // until this arm returns.
        Coalesced::Produce(_guard) => {
            let mut frame = produce_local()?;
            let unstamped = natively_unstamped(&frame);
            // A mid-tick `list-panes` race can drop a live pane's command/cwd/
            // process-start; rather than fold an anonymous `external`/`process`
            // row that blinks out next tick, backfill the missing fields from
            // the last good read of the same pane id.
            rotate_from_cache(&mut frame, &cache_path, session);
            // Enrich each pane with per-process resource metrics (best-effort,
            // Linux-only). Runs inside the produce lock so only one producer
            // reads `/proc` per tick; the result is in the published pane cache,
            // so consumer tabs never fork their own reads.
            super::metrics::enrich_pane_metrics(&mut frame, session, runtime);
            // Metrics may bind a Zellij pane to its root pid; use that live
            // pid to repair a raced-empty cwd before publishing the frame.
            backfill_pane_cwds(&mut frame, &|pid| crate::proc::cwd(pid));
            // Stamp the in-pane agent process starts before the publish — after
            // the enrich, whose pane→root-pid bindings the stamp's first rung
            // rides — so the cache carries them to every reader: the in-process
            // produce and the consumer in-process fold alike.
            stamp_pane_process_starts(
                &mut frame,
                &unstamped,
                &crate::remote_control::in_pane_agent_start_for_root,
                &crate::remote_control::in_pane_agent_start,
            );
            if let Err(err) = atomic::write_temp_then_rename_cache(&cache_path, &frame) {
                tracing::warn!(path = %cache_path.display(), error = %err, "sidebar snapshot cache write failed");
            } else if let Err(err) =
                crate::ledger::wakeup::wake_sidebars_pane_frame_published(runtime)
            {
                tracing::debug!(error = %err, "sidebar pane-frame publication wakeup failed");
            }
            Ok(frame)
        }
    }
}

#[cfg(test)]
mod tests;
