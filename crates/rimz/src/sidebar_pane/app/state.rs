//! Pure reducers and unread-fold helpers for the sidebar serve loop.
//!
//! `LoopState` owns fetch-outcome application; this module keeps the testable
//! state reducer, diagnostics projection, and read-receipt helpers it calls.

use std::collections::HashMap;

use crate::SidebarSnapshot;
use crate::diag::record::{DiagEvent, GroupIdentity};
use crate::ids::PaneId;
use jiff::Timestamp;

use crate::sidebar::unread::{self, ClearedUnread, UnreadClearCause};
use crate::sidebar_pane::render::UiState;

use super::gate::{GateState, gate_held_ms};
use super::health::{Health, next_health};
use super::selection::row_index_of_pane;

/// Decide what to render next given the latest snapshot outcome.
/// Pure data, no I/O — extracted so the loop's recovery rules are testable.
pub(super) fn compute_next_state(
    snapshot: std::result::Result<SidebarSnapshot, String>,
    committed: &SidebarSnapshot,
    previous_health: &Health,
) -> RenderState {
    match snapshot {
        Ok(snapshot) => RenderState {
            snapshot,
            health: next_health(previous_health, None),
        },
        Err(reason) => RenderState {
            snapshot: committed.clone(),
            health: next_health(previous_health, Some(format!("snapshot failed: {reason}"))),
        },
    }
}

/// Bundle returned by [`compute_next_state`]; the loop applies it verbatim.
#[derive(Clone, Debug)]
pub(super) struct RenderState {
    pub(super) snapshot: SidebarSnapshot,
    pub(super) health: Health,
}

/// What [`apply_fetch_outcome`] reports back to the loop: whether to exit,
/// whether that exit is the tab-empty self-close path, and whether this fetch
/// was held as a transient regression (the loop fires one self-heal refetch so
/// the cache reaches the next good frame).
pub(super) struct ApplyOutcome {
    pub(super) should_exit: bool,
    pub(super) tab_emptied: bool,
    pub(super) rejected: bool,
}

pub(super) struct FetchDiagnostics<'a> {
    pub(super) prev_snapshot: &'a SidebarSnapshot,
    pub(super) incoming_panes_produced_at_ms: Option<u64>,
    pub(super) next_snapshot: &'a SidebarSnapshot,
    pub(super) prev_health: &'a Health,
    pub(super) next_health: &'a Health,
    pub(super) prev_gate: &'a GateState,
    pub(super) next_gate: &'a GateState,
    pub(super) fetch_failure: Option<String>,
    pub(super) rejected: bool,
    pub(super) released_via_escape_hatch: bool,
    pub(super) is_elder: bool,
    pub(super) now: Timestamp,
}

pub(super) fn emit_diagnostics(diag: &crate::diag::DiagSink, diagnostics: FetchDiagnostics<'_>) {
    let FetchDiagnostics {
        prev_snapshot,
        incoming_panes_produced_at_ms,
        next_snapshot,
        prev_health,
        next_health,
        prev_gate,
        next_gate,
        fetch_failure,
        rejected,
        released_via_escape_hatch,
        is_elder,
        now,
    } = diagnostics;
    if let Some(reason) = fetch_failure {
        diag.emit(crate::diag::record::DiagEvent::FetchFailure {
            reason,
            failure_streak: next_health.failure_streak,
        });
    }
    if rejected && let Some(rule) = next_gate.rule {
        diag.emit(crate::diag::record::DiagEvent::GateHold {
            rule,
            prev_produced_at_ms: prev_snapshot.panes_produced_at_ms,
            incoming_produced_at_ms: incoming_panes_produced_at_ms,
            reject_streak: next_gate.reject_streak,
        });
    } else if next_gate.rule.is_none()
        && let Some(rule) = prev_gate.rule
    {
        diag.emit(crate::diag::record::DiagEvent::GateRelease {
            rule,
            held_ms: gate_held_ms(prev_gate, now),
            via_escape_hatch: released_via_escape_hatch,
        });
    }
    match (prev_health.alert.as_ref(), next_health.alert.as_ref()) {
        (prev, Some(next)) if next.is_active() && !prev.is_some_and(|alert| alert.is_active()) => {
            diag.emit_unlimited(crate::diag::record::DiagEvent::HealthAlert {
                reason: next.reason.clone(),
                since_ms: next.since.as_millisecond().max(0) as u64,
                recovered_after_ms: None,
            });
        }
        (Some(prev), Some(next)) if prev.is_active() && !next.is_active() => {
            let recovered_after_ms = next.recovered_at.and_then(|recovered| {
                recovered
                    .duration_since(next.since)
                    .as_millis()
                    .try_into()
                    .ok()
            });
            diag.emit_unlimited(crate::diag::record::DiagEvent::HealthAlert {
                reason: next.reason.clone(),
                since_ms: next.since.as_millisecond().max(0) as u64,
                recovered_after_ms,
            });
        }
        _ => {}
    }
    if is_elder {
        for event in diff_group_migrations(prev_snapshot, next_snapshot) {
            diag.emit(event);
        }
    }
}

fn diff_group_migrations(prev: &SidebarSnapshot, next: &SidebarSnapshot) -> Vec<DiagEvent> {
    let prev_rows = rows_by_pane(prev);
    let next_rows = rows_by_pane(next);
    let mut events = Vec::new();
    for (pane_id, next_group) in next_rows {
        let Some(prev_group) = prev_rows.get(&pane_id) else {
            continue;
        };
        // A group migration is a pane changing cwd across a group boundary.
        // A cwd that changes while the group identity holds (e.g. a worktree
        // pane whose cwd flaps between two paths that both fold to `external`)
        // is not a migration, and neither is a stable-cwd reclassification
        // while a newborn worktree pane settles from `external` to `worktree`.
        if prev_group.group == next_group.group {
            continue;
        }
        if prev_group.cwd == next_group.cwd {
            continue;
        }
        events.push(DiagEvent::GroupMigration {
            pane_id,
            from: prev_group.group.clone(),
            to: next_group.group,
            cwd_before: prev_group.cwd.clone(),
            cwd_after: next_group.cwd,
        });
    }
    events
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RowLocation {
    group: GroupIdentity,
    cwd: Option<String>,
}

fn rows_by_pane(snapshot: &SidebarSnapshot) -> HashMap<PaneId, RowLocation> {
    let mut rows = HashMap::new();
    for group in &snapshot.worktree_groups {
        let identity = GroupIdentity {
            kind: worktree_kind_name(group.kind).to_owned(),
            key: group.key.clone(),
        };
        for row in &group.rows {
            let Some(pane) = row.pane.as_ref() else {
                continue;
            };
            rows.insert(
                pane.pane_id.clone(),
                RowLocation {
                    group: identity.clone(),
                    cwd: pane.cwd.clone(),
                },
            );
        }
    }
    rows
}

fn worktree_kind_name(kind: crate::SidebarWorktreeKind) -> &'static str {
    match kind {
        crate::SidebarWorktreeKind::Channel => "channel",
        crate::SidebarWorktreeKind::Worktree => "worktree",
        crate::SidebarWorktreeKind::Root => "root",
        crate::SidebarWorktreeKind::External => "external",
    }
}

pub(super) fn session_focus_baseline(
    snapshot: &SidebarSnapshot,
    own_pane: Option<&crate::ids::PaneId>,
) -> Option<crate::ids::PaneId> {
    snapshot
        .focused_pane
        .as_ref()
        .filter(|pane| own_pane != Some(*pane))
        .filter(|pane| row_index_of_pane(snapshot, None, pane).is_some())
        .cloned()
}

pub(super) fn apply_manual_unread_guard(
    ui: &mut UiState,
    focused_row_id: Option<&str>,
    focus_clear: &mut ReadClear,
) {
    let Some(guarded) = ui.unread_guard.clone() else {
        return;
    };
    if focused_row_id == Some(guarded.as_str()) {
        focus_clear.ids.retain(|id| id != &guarded);
        focus_clear.trace.retain(|change| change.row_id != guarded);
    } else {
        ui.unread_guard = None;
    }
}

pub(super) fn row_id_of_pane(
    snapshot: &SidebarSnapshot,
    pane_id: &crate::ids::PaneId,
) -> Option<String> {
    row_of_pane(snapshot, pane_id).map(|row| row.id.clone())
}

pub(super) fn row_of_pane<'a>(
    snapshot: &'a SidebarSnapshot,
    pane_id: &crate::ids::PaneId,
) -> Option<&'a crate::SidebarRow> {
    snapshot.rows().find(|row| {
        row.pane
            .as_ref()
            .is_some_and(|pane| pane.pane_id == *pane_id)
    })
}

#[derive(Clone, Debug, Default)]
pub(super) struct ReadClear {
    pub(super) ids: Vec<String>,
    pub(super) trace: Vec<ClearedUnread>,
}

impl ReadClear {
    pub(super) fn merge(&mut self, other: Self) {
        for id in other.ids {
            if !self.ids.iter().any(|seen| seen == &id) {
                self.ids.push(id);
            }
        }
        self.trace.extend(other.trace);
    }
}

/// The read receipt to write for a row that was just read by one of the
/// renderer/CLI clear paths. Returns the row id to clear and, when the row was
/// unread, the clear trace. An empty result means the row is already read or
/// never needed a look, so nothing is written.
pub(super) fn read_receipt_for_row(
    snapshot: &SidebarSnapshot,
    row_id: Option<&str>,
    cause: UnreadClearCause,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let Some(row_id) = row_id else {
        return ReadClear::default();
    };
    let Some(row) = snapshot.rows().find(|row| row.id == row_id) else {
        return ReadClear::default();
    };
    read_receipt_for_row_ref(row, cause, marks, now)
}

fn read_receipt_for_row_ref(
    row: &crate::SidebarRow,
    cause: UnreadClearCause,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let needs_look = row
        .status()
        .is_some_and(crate::agents::AgentStatus::needs_a_look);
    if !row.unread && (!needs_look || unread::receipt_reaches(marks, &row.id, row.last_activity)) {
        return ReadClear::default();
    }
    let trace = row
        .unread
        .then(|| unread::cleared_unread(row, cause, Some(now.as_millisecond())))
        .into_iter()
        .collect();
    ReadClear {
        ids: vec![row.id.clone()],
        trace,
    }
}

/// Read receipts for unread rows whose pane shares the renderer's own tab,
/// excluding the focused row because the focus path already handled it.
pub(super) fn read_receipts_for_tab(
    snapshot: &SidebarSnapshot,
    working_pane_ids: &[crate::ids::PaneId],
    focused_row_id: Option<&str>,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let mut clear = ReadClear::default();
    for row in snapshot.rows() {
        if Some(row.id.as_str()) == focused_row_id {
            continue;
        }
        let in_tab = row
            .pane
            .as_ref()
            .is_some_and(|pane| working_pane_ids.contains(&pane.pane_id));
        if in_tab {
            clear.merge(read_receipt_for_row_ref(
                row,
                UnreadClearCause::TabView,
                marks,
                now,
            ));
        }
    }
    clear
}

/// Read receipts for every row a manual "mark all read" sweep covers.
pub(super) fn read_receipts_for_all(
    snapshot: &SidebarSnapshot,
    cause: UnreadClearCause,
    marks: &crate::sidebar::read_marks::ReadMarks,
    now: Timestamp,
) -> ReadClear {
    let mut clear = ReadClear::default();
    for row in snapshot.rows() {
        clear.merge(read_receipt_for_row_ref(row, cause, marks, now));
    }
    clear
}

/// Set the `unread` bit on the named rows in place — the instant local feedback
/// for a focus/mark-read clear (`false`) or a mark-unread re-flag (`true`),
/// ahead of the durable write the next produce re-derives.
pub(super) fn set_rows_unread(snapshot: &mut SidebarSnapshot, ids: &[String], unread: bool) {
    if ids.is_empty() {
        return;
    }
    for row in snapshot.rows_mut() {
        if ids.iter().any(|id| id == &row.id) {
            row.unread = unread;
        }
    }
}

pub(super) fn emit_unread_cleared_trace(diag: &crate::diag::DiagSink, changes: &[ClearedUnread]) {
    for change in changes {
        diag.trace_notify(change.trace_event());
    }
}

#[cfg(test)]
mod tests;
