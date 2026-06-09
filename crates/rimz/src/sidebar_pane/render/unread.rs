use std::collections::{HashMap, HashSet};

use crate::SidebarRow;
use crate::feed::AgentStatus;

/// Per-renderer unread/read state keyed by durable sidebar row id.
///
/// The first frame records a baseline without flagging, so attaching a sidebar
/// to an already-busy room does not open as a wall of unread cards. Later
/// `Running -> needs-a-look` transitions set unread, and focusing the row's pane
/// clears it after the transition fold so "finished while focused" nets read.
#[derive(Clone, Debug, Default)]
pub(crate) struct UnreadTracker {
    prev: HashMap<String, AgentStatus>,
    unread: HashSet<String>,
}

impl UnreadTracker {
    pub(crate) fn observe<'a>(
        &mut self,
        rows: impl Iterator<Item = &'a SidebarRow>,
        focused_row_id: Option<&str>,
    ) {
        let mut live = HashSet::new();
        for row in rows {
            let Some(status) = row.status() else {
                continue;
            };
            live.insert(row.id.clone());
            match self.prev.insert(row.id.clone(), status) {
                None => {}
                Some(AgentStatus::Running)
                    if matches!(
                        status,
                        AgentStatus::Success
                            | AgentStatus::Failed
                            | AgentStatus::Waiting
                            | AgentStatus::Paused
                    ) =>
                {
                    self.unread.insert(row.id.clone());
                }
                Some(_) if matches!(status, AgentStatus::Running | AgentStatus::Idle) => {
                    self.unread.remove(&row.id);
                }
                Some(_) => {}
            }
        }
        self.prev.retain(|key, _| live.contains(key));
        self.unread.retain(|key| live.contains(key));

        if let Some(id) = focused_row_id {
            self.unread.remove(id);
        }
    }

    pub(crate) fn is_unread(&self, id: &str) -> bool {
        self.unread.contains(id)
    }
}

#[cfg(test)]
mod tests;
