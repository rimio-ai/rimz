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

/// The event-fresh ledger rollup, read in process: `latest.json` when it
/// reflects the log (lock-free, O(snapshot)), else a re-projection folded
/// through the caller's [`RollupCursor`] — O(new log bytes) per delta from
/// the in-memory base, and a fresh cursor folds cold, so a one-shot caller
/// just passes `&mut RollupCursor::new()`. The read-only twin of the
/// producer's `Ledger::snapshot_cached`, exposed so a consumer tab folds the
/// freshest rollup over the producer's coalesced panes without holding a
/// writer handle — the rollup is what makes a status change or a new agent in
/// an existing pane repaint within one wakeup, independent of the slower
/// pane-list cadence. `Err` preserves *why* the ledger was unreadable (a torn
/// frame, a permissions failure): the serve loop treats it as a soft miss —
/// hold the last good frame, name the cause on the health line — where a
/// produce or inspection call propagates it.
pub fn rollup_snapshot(
    state: &StatePaths,
    cursor: &mut RollupCursor,
) -> crate::ledger::snapshot::Result<SidebarSnapshot> {
    match crate::ledger::snapshot::read_fresh_latest(state) {
        Some(snapshot) => Ok(snapshot),
        None => crate::ledger::snapshot::build_with_cursor(state, cursor),
    }
}

/// Render the consumer snapshot entirely from runtime caches and sidecars — no
/// `list-panes`, no git. Reads the **event-fresh** rollup in process from
/// `latest.json` (`consumer_rollup`), folds the producer's coalesced pane list
/// from `snapshot.json` when one exists, folds the session and subagent
/// statusline context plus per-tool activity, overlays the panes with this
/// renderer's own-pane exclusion, and projects the cached diff stats. Before
/// the producer's first pane-frame publish, the fold is intentionally
/// frameless: `panes_produced_at_ms == None` and no pane-admitted cards render,
/// while ledger metadata can still paint. `Err` means the ledger rollup itself
/// was unreadable and carries why; the serve loop holds its last good frame
/// and surfaces the reason.
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
) -> crate::ledger::snapshot::Result<SidebarSnapshot> {
    let base = rollup_snapshot(state, cursor)?;
    let cache = read_snapshot_cache(&runtime.root.join("snapshot.json"), session);
    Ok(enrich(
        base,
        cache,
        runtime,
        exclude,
        EnrichMode::Cached,
        None,
    ))
}
