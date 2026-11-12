//! The room's final-fold reconciliation of its runtime records.
//!
//! The host decides when a fold is final; this module decides which records
//! reconcile on it and in what order. It is the one route from outside
//! `sidebar/` to those reconcilers.

use crate::disk::paths::RuntimePaths;
use crate::sidebar::unread::ReconcileOutcome;
use crate::store::snapshot::SidebarSnapshot;

/// Reconciles a final fold's attention stamps, then its unread episodes, and
/// returns the unread outcome. Groups re-sort after both, so their order and
/// `group.inactive` read the stamps and `unread` bits this fold wrote. A
/// frameless snapshot writes neither record, and an `unread.lock` held past its
/// wait returns the empty outcome, so those opens notify on the next final fold.
pub(crate) fn reconcile_final_fold(
    runtime: &RuntimePaths,
    snapshot: &mut SidebarSnapshot,
) -> ReconcileOutcome {
    crate::sidebar::attention::reconcile_snapshot(runtime, snapshot);
    let unread = crate::sidebar::unread::reconcile_snapshot(runtime, snapshot);
    snapshot.sort_groups_for_presentation();
    unread
}

#[cfg(test)]
mod tests;
