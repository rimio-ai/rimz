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
/// `Running -> needs-a-look` transitions set unread with the row's
/// `last_activity` as the episode stamp. A receipt clears only episodes whose
/// stamp it reaches, so an old focus clear never eats a later turn. Focusing the
/// row's pane clears it after receipt application so "finished while focused"
/// nets read and emits one receipt for peers.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnreadTracker {
    prev: HashMap<String, AgentStatus>,
    unread: HashMap<String, Timestamp>,
}

impl UnreadTracker {
    pub(crate) fn observe<'a>(
        &mut self,
        rows: impl Iterator<Item = &'a SidebarRow>,
        focused_row_id: Option<&str>,
        marks: &ReadMarks,
    ) -> Vec<String> {
        let mut live = HashSet::new();
        let mut focused_receipt = None;
        for row in rows {
            let Some(status) = row.status() else {
                continue;
            };
            live.insert(row.id.clone());
            if focused_row_id == Some(row.id.as_str())
                && status_can_clear_unread(status)
                && !receipt_reaches(marks, &row.id, row.last_activity)
            {
                focused_receipt = Some(row.id.clone());
            }
            match self.prev.insert(row.id.clone(), status) {
                None => {}
                Some(AgentStatus::Running) if status_can_clear_unread(status) => {
                    self.unread.insert(row.id.clone(), row.last_activity);
                }
                Some(_) if matches!(status, AgentStatus::Running | AgentStatus::Idle) => {
                    self.unread.remove(&row.id);
                }
                Some(_) => {}
            }
        }
        self.prev.retain(|key, _| live.contains(key));
        self.unread
            .retain(|key, stamp| live.contains(key) && !receipt_reaches(marks, key, *stamp));

        let mut cleared = Vec::new();
        if let Some(id) = focused_receipt {
            self.unread.remove(&id);
            cleared.push(id);
        }
        cleared
    }

    pub(crate) fn is_unread(&self, id: &str) -> bool {
        self.unread.contains_key(id)
    }
}

fn status_can_clear_unread(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Success | AgentStatus::Failed | AgentStatus::Waiting | AgentStatus::Paused
    )
}

fn receipt_reaches(marks: &ReadMarks, row_id: &str, stamp: Timestamp) -> bool {
    marks
        .cleared_at_ms(row_id)
        .is_some_and(|cleared_at_ms| cleared_at_ms >= stamp.as_millisecond())
}

#[cfg(test)]
mod tests;
