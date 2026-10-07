//! Pure tab-status projection from the producer's fused agent rows and pane
//! frame. Mux mutation stays with the elected producer; this module only
//! decides which observed names need a suffix, survivor label, or shell release.

use std::collections::{HashMap, HashSet};

use crate::agents::AgentStatus;
use crate::ids::PaneId;
use crate::mux::tab_name::{
    TabNameIntent, TabOwnerRecord, ViewNaming, is_scoped_label, label_from_pane_names, stored_label,
};
use crate::sidebar::frame::{PaneFrame, PaneState};
use crate::sidebar::timing::TAB_SUCCESS_STATUS_TTL;
use crate::store::snapshot::SidebarSnapshot;
use crate::theme;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRename {
    pub(crate) anchor: PaneId,
    pub(crate) desired_name: String,
    pub(crate) intent: TabNameIntent,
}

/// Declaration order is the product precedence ladder consumed by `max`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TabStatus {
    Success,
    Running,
    Paused,
    Waiting,
    Failed,
}

impl TabStatus {
    fn from_row(status: AgentStatus, fresh_success: bool) -> Option<Self> {
        match status {
            AgentStatus::Failed => Some(Self::Failed),
            AgentStatus::Waiting => Some(Self::Waiting),
            AgentStatus::Paused => Some(Self::Paused),
            AgentStatus::Running => Some(Self::Running),
            AgentStatus::Success if fresh_success => Some(Self::Success),
            AgentStatus::Success | AgentStatus::Idle | AgentStatus::Sleeping => None,
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Failed => "!",
            Self::Waiting => "?",
            Self::Paused => "⏸\u{FE0E}",
            Self::Running => "⢿",
            Self::Success => "✓",
        }
    }
}

enum TabOwner<'a> {
    Automatic,
    User,
    Rimz(&'a TabOwnerRecord),
}

fn classify<'a>(naming: &'a ViewNaming, base: &str) -> TabOwner<'a> {
    match naming.owner.as_ref() {
        Some(record) if record.base == base => TabOwner::Rimz(record),
        None if naming.automatic => TabOwner::Automatic,
        _ => TabOwner::User,
    }
}

pub(crate) fn desired_tab_renames(
    snapshot: &SidebarSnapshot,
    frame: &PaneFrame,
    shell_name: &str,
) -> Vec<TabRename> {
    let live_agent_panes = snapshot
        .rows()
        .filter(|row| row.as_agent().is_some())
        .filter_map(|row| row.pane.as_ref().map(|pane| &pane.pane_id))
        .collect::<HashSet<_>>();
    let status_by_pane = snapshot
        .rows()
        .filter_map(|row| {
            let pane = row.pane.as_ref()?;
            let status = row.status()?;
            let success_age = snapshot.now.duration_since(row.last_activity);
            let fresh_success = success_age <= TAB_SUCCESS_STATUS_TTL;
            TabStatus::from_row(status, fresh_success).map(|status| (pane.pane_id.clone(), status))
        })
        .fold(
            HashMap::<PaneId, TabStatus>::new(),
            |mut statuses, (pane, status)| {
                statuses
                    .entry(pane)
                    .and_modify(|known| *known = (*known).max(status))
                    .or_insert(status);
                statuses
            },
        );
    frame
        .tabs
        .iter()
        .filter_map(|tab| {
            let observed_name = tab.name.as_ref()?;
            if observed_name == crate::pane::VIEW_NAME {
                return None;
            }
            let base = theme::strip_status_glyph_suffix(observed_name, &snapshot.theme);
            let owner = classify(&tab.naming, base);
            if matches!(owner, TabOwner::Automatic) {
                return None;
            }
            let work_panes = tab
                .panes
                .iter()
                .filter(|pane| {
                    !pane.current.command.as_deref().is_some_and(|command| {
                        crate::pane::command_is_sidebar_chrome(command)
                            || crate::pane::command_is_host(command)
                    }) && !pane
                        .current
                        .spawn_command
                        .as_deref()
                        .is_some_and(crate::pane::command_is_host)
                })
                .collect::<Vec<_>>();
            let mut anchor = work_panes
                .first()
                .copied()
                .or_else(|| tab.panes.first())?
                .pane_id
                .clone();
            let status = work_panes
                .iter()
                .filter_map(|pane| status_by_pane.get(&pane.pane_id).copied())
                .max();
            let is_live_agent = |pane: &PaneState| {
                live_agent_panes.contains(&pane.pane_id) || pane.current.hosted_agent_kind.is_some()
            };
            let observed = observed_name.clone();
            let (mut desired_name, mut intent) = if let Some(status) = status {
                (
                    format!("{base} {}", status.glyph()),
                    TabNameIntent::Status {
                        observed: observed.clone(),
                    },
                )
            } else {
                (
                    base.to_owned(),
                    TabNameIntent::Rest {
                        observed: observed.clone(),
                    },
                )
            };
            if let TabOwner::Rimz(record) = owner
                && !is_scoped_label(base)
                && !work_panes
                    .iter()
                    .any(|pane| record.founders.contains(&pane.pane_id) && is_live_agent(pane))
            {
                let mut survivors = work_panes
                    .into_iter()
                    .filter(|pane| is_live_agent(pane))
                    .collect::<Vec<_>>();
                survivors.sort_by_key(|pane| {
                    (
                        pane.first_seen_at_ms.is_none(),
                        pane.first_seen_at_ms.unwrap_or_default(),
                        pane.pane_id.as_str(),
                    )
                });
                if let Some(first) = survivors.first() {
                    anchor = first.pane_id.clone();
                    let label = stored_label(
                        tab.kind,
                        &label_from_pane_names(
                            survivors.iter().filter_map(|pane| pane.title.as_deref()),
                        ),
                    );
                    if !label.is_empty() && label != base {
                        desired_name = status.map_or_else(
                            || label.clone(),
                            |status| format!("{label} {}", status.glyph()),
                        );
                        intent = TabNameIntent::Rebuild {
                            observed,
                            base: label,
                        };
                    }
                } else {
                    desired_name = shell_name.to_owned();
                    intent = if base == shell_name {
                        TabNameIntent::Rest { observed }
                    } else {
                        TabNameIntent::Release { observed }
                    };
                }
            }
            (desired_name != *observed_name).then_some(TabRename {
                anchor,
                desired_name,
                intent,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
