//! Consumer-side snapshot read: fresh ledger rollup over the producer pane cache.
//!
//! Renderers that are not the elected producer stay in this lane: no mux call,
//! no git call, no provider probe, and no durable ledger writes.

use crate::ids::PaneId;
use crate::{RuntimePaths, SidebarSnapshot, StatePaths};

use super::cache::read_snapshot_cache;
use super::enrich::{EnrichMode, enrich};

/// Re-exported for long-lived consumers (the sidebar fetch worker), which sit
/// behind this module's read-only boundary and never import `crate::ledger`.
pub use crate::ledger::snapshot::RollupCursor;

/// The event-fresh ledger rollup for a consumer, read in process: `latest.json`
/// when it reflects the log (lock-free, O(snapshot)), else a re-projection
/// folded through the caller's [`RollupCursor`] — O(new log bytes) per delta
/// from the in-memory base, and a fresh cursor folds cold, so a one-shot
/// caller just passes `&mut RollupCursor::new()`. The read-only twin of the
/// producer's `Ledger::snapshot_cached`, exposed so a consumer tab folds the
/// freshest rollup over the producer's coalesced panes without holding a
/// writer handle — the rollup is what makes a status change or a new agent in
/// an existing pane repaint within one wakeup, independent of the slower
/// pane-list cadence. `None` only when the ledger itself is unreadable, which
/// the caller treats as a soft miss and holds the last good frame.
fn consumer_rollup(state: &StatePaths, cursor: &mut RollupCursor) -> Option<SidebarSnapshot> {
    rollup_snapshot(state, cursor).ok()
}

/// [`consumer_rollup`] with the error chain preserved: the produce pipeline and
/// one-shot CLI reads surface *why* the ledger was unreadable (a torn frame, a
/// permissions failure) instead of folding it into a soft miss. One
/// implementation under both shapes — the loop-shaped consumer read swallows
/// the error because its recovery is "hold the last good frame", where a
/// produce or inspection call reports it.
pub fn rollup_snapshot(
    state: &StatePaths,
    cursor: &mut RollupCursor,
) -> crate::ledger::snapshot::Result<SidebarSnapshot> {
    match crate::ledger::snapshot::read_fresh_latest(state) {
        Some(snapshot) => Ok(snapshot),
        None => crate::ledger::snapshot::build_with_cursor(state, cursor),
    }
}

/// Render the published snapshot for a consumer renderer, entirely from runtime
/// caches and sidecars — no `list-panes`, no git. Reads the producer's coalesced
/// pane list from `snapshot.json`, pairs it with the **event-fresh** rollup read
/// in process from `latest.json` (`consumer_rollup`), folds the session and
/// subagent statusline context plus per-tool activity, overlays the panes with
/// this renderer's own-pane exclusion, and projects the cached diff stats. `None`
/// until the producer has published a pane set (or if the ledger is unreadable),
/// so the caller holds its last good frame.
///
/// Pairing fresh rollup + coalesced panes is the lag fix: a `LedgerDelta` folds
/// the new agent/status in this tab within one wakeup, while the slower
/// `list-panes` cadence only governs genuine pane open/close.
///
/// This is the producer's fast-lane twin: the native renderer calls it directly
/// each tick, and the `--no-produce` CLI path (the plugin rail's read) shares it.
///
/// The rollup folds through the caller's [`RollupCursor`], so a long-lived
/// reader (the sidebar fetch worker owns one across its loop) pays O(new log
/// bytes) per wakeup instead of a full `rollup.json` re-read; a fresh cursor
/// folds cold, so a one-shot caller passes `&mut RollupCursor::new()`.
pub fn read_published_snapshot(
    cursor: &mut RollupCursor,
    state: &StatePaths,
    runtime: &RuntimePaths,
    session: &str,
    exclude: Option<&PaneId>,
) -> Option<SidebarSnapshot> {
    let cache_path = runtime.root.join("snapshot.json");
    let cache = read_snapshot_cache(&cache_path, session)?;
    let base = consumer_rollup(state, cursor)?;
    Some(enrich(
        base,
        Some(cache),
        runtime,
        exclude,
        EnrichMode::Cached,
    ))
}
