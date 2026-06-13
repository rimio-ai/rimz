use std::collections::{HashMap, HashSet};

use crate::SidebarRow;
use crate::feed::AgentStatus;
use crate::sidebar::read_marks::ReadMarks;
use jiff::Timestamp;

/// Per-renderer unread/read state keyed by durable sidebar row id, with runtime
/// read receipts merged in from peer renderers.
///
/// The first frame records a baseline without flagging, so attaching a sidebar
/// to an already-busy room does not open as a wall of unread cards. Later
/// transitions into a needs-a-look state set unread with the row's
/// `last_activity` as the episode stamp. A receipt clears only episodes whose
/// stamp it reaches, so an old focus clear never eats a later turn. Focusing the
/// row's pane clears it after receipt application so "finished while focused"
/// nets read and emits one receipt for peers.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnreadTracker {
    seeded: bool,
    prev: HashMap<String, AgentStatus>,
    unread: HashMap<String, Timestamp>,
}

impl UnreadTracker {
    pub(crate) fn observe<'a>(
        &mut self,
        rows: impl Iterator<Item = &'a SidebarRow>,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
    ) -> ObserveOutcome {
        let mut live = HashSet::new();
        let mut focused_receipt = None;
        let mut changes = Vec::new();
        let seeded = self.seeded;
        for row in rows {
            let Some(status) = row.status() else {
                continue;
            };
            live.insert(row.id.clone());
            if focused_row_id == Some(row.id.as_str())
                && status.needs_a_look()
                && !receipt_reaches(marks, &row.id, row.last_activity)
            {
                focused_receipt = Some(row.id.clone());
            }
            match self.prev.insert(row.id.clone(), status) {
                None if seeded && status.marks_unread() => {
                    self.unread.insert(row.id.clone(), row.last_activity);
                    changes.push(UnreadChange::marked(&row.id, status));
                }
                None => {}
                Some(prev) if prev != status && status.marks_unread() => {
                    self.unread.insert(row.id.clone(), row.last_activity);
                    changes.push(UnreadChange::marked(&row.id, status));
                }
                Some(_) => {}
            }
        }
        self.seeded = true;
        self.prev.retain(|key, _| live.contains(key));
        // Unread is sticky: it clears only when the row vanishes or a read
        // receipt reaches its episode — never on the agent's own return to a
        // running/idle state. A pending look stays pending until a human looks.
        self.unread.retain(|key, stamp| {
            if live.contains(key) && !receipt_reaches(marks, key, *stamp) {
                return true;
            }
            let cause = if live.contains(key) {
                ClearCause::Receipt
            } else {
                ClearCause::RowGone
            };
            changes.push(UnreadChange::cleared(key, cause));
            false
        });

        let mut focus_cleared = Vec::new();
        if let Some(id) = focused_receipt {
            if self.unread.remove(&id).is_some() {
                changes.push(UnreadChange::cleared(&id, ClearCause::Focus));
            }
            focus_cleared.push(id);
        }
        ObserveOutcome {
            focus_cleared,
            changes,
        }
    }

    pub(crate) fn is_unread(&self, id: &str) -> bool {
        self.unread.contains_key(id)
    }
}

/// What a single [`UnreadTracker::observe`] fold produced: the focus-cleared row
/// ids that become read receipts for peer renderers, plus the typed unread
/// changes the caller appends to the notification trace.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ObserveOutcome {
    pub focus_cleared: Vec<String>,
    pub changes: Vec<UnreadChange>,
}

/// A single unread mark or clear observed this fold, for the notification trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnreadChange {
    pub row_id: String,
    pub kind: UnreadChangeKind,
}

impl UnreadChange {
    fn marked(row_id: &str, status: AgentStatus) -> Self {
        Self {
            row_id: row_id.to_owned(),
            kind: UnreadChangeKind::Marked(status),
        }
    }

    fn cleared(row_id: &str, cause: ClearCause) -> Self {
        Self {
            row_id: row_id.to_owned(),
            kind: UnreadChangeKind::Cleared(cause),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnreadChangeKind {
    /// The row reached a pending-look status on its own.
    Marked(AgentStatus),
    /// A pending look was cleared.
    Cleared(ClearCause),
}

/// Why a pending look cleared. Under sticky semantics the only clears are a
/// human look (focus or a peer read receipt) or the row disappearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ClearCause {
    /// A read receipt reached the unread episode (this renderer focused it
    /// earlier, or a peer renderer published the clear).
    Receipt,
    /// This renderer's own pane focus cleared it this fold.
    Focus,
    /// The row left the snapshot before anyone looked.
    RowGone,
}

impl ClearCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Receipt => "receipt",
            Self::Focus => "focus",
            Self::RowGone => "row_gone",
        }
    }
}

fn receipt_reaches(marks: &ReadMarks, row_id: &str, stamp: Timestamp) -> bool {
    marks
        .cleared_at_ms(row_id)
        .is_some_and(|cleared_at_ms| cleared_at_ms >= stamp.as_millisecond())
}

#[cfg(test)]
mod tests;
