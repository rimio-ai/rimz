use serde::Serialize;

pub use crate::schema::diag::{EventPaneSig, EventsSig, StatusCountSig};
use crate::schema::sidebar_event::SidebarEvent;
use crate::sidebar::events::EventStore;
use crate::{SidebarSnapshot, SidebarWorktreeKind};

use super::WatchedField;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameSig {
    pub at_ms: u64,
    pub panes_produced_at_ms: Option<u64>,
    pub rows: Vec<RowSig>,
    pub groups: Vec<GroupSig>,
    pub own_view: Option<OwnViewSig>,
    pub events: EventsSig,
    pub pulled_rows: usize,
    pub pulled_panes_produced_at_ms: Option<u64>,
    pub gate_reject_streak: u32,
    pub health_failure_streak: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RowSig {
    pub row_id: String,
    pub is_agent: bool,
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub pane_process_start: Option<jiff::Timestamp>,
    pub group_key: String,
    pub watched: WatchedValues,
    pub sub_agent_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WatchedValues {
    pub status: Option<String>,
    pub context_pct: Option<u8>,
    pub total_tokens: Option<u64>,
    pub group_key: String,
    pub model: Option<String>,
}

impl WatchedValues {
    pub fn fields(&self) -> Vec<(WatchedField, Option<String>)> {
        vec![
            (WatchedField::Status, self.status.clone()),
            (
                WatchedField::ContextPct,
                self.context_pct.map(|value| value.to_string()),
            ),
            (
                WatchedField::TotalTokens,
                self.total_tokens.map(|value| value.to_string()),
            ),
            (WatchedField::GroupKey, Some(self.group_key.clone())),
            (WatchedField::Model, self.model.clone()),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupSig {
    pub key: String,
    pub kind: SidebarWorktreeKind,
    pub row_ids: Vec<String>,
    pub hidden_count: usize,
    pub status_counts: Vec<StatusCountSig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnViewSig {
    pub sibling_count: usize,
    pub active_pane_id: Option<String>,
    pub working_pane_ids: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct RosterSig {
    pub panes_produced_at_ms: Option<u64>,
    pub rows: Vec<RosterRowSig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRowSig {
    pub row_id: String,
    pub is_agent: bool,
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub pane_process_start: Option<jiff::Timestamp>,
}

pub fn extract_sig(
    current: &SidebarSnapshot,
    last_pulled: &SidebarSnapshot,
    event_store: &EventStore,
    gate_reject_streak: u32,
    health_failure_streak: u32,
    now_ms: u64,
) -> FrameSig {
    let mut rows = current
        .worktree_groups
        .iter()
        .flat_map(|group| {
            group.rows.iter().map(|row| RowSig {
                row_id: row.id.clone(),
                is_agent: row.is_agent(),
                pane_id: row.pane.as_ref().map(|pane| pane.pane_id.to_string()),
                pane_pid: row.pane.as_ref().and_then(|pane| pane.pane_pid),
                pane_process_start: row.pane.as_ref().and_then(|pane| pane.pane_process_start),
                group_key: group.key.clone(),
                watched: WatchedValues {
                    status: row.status().map(|status| status.as_str().to_owned()),
                    context_pct: row.context_gauge_percent(),
                    total_tokens: row.total_tokens(),
                    group_key: group.key.clone(),
                    model: row.model().map(ToOwned::to_owned),
                },
                sub_agent_ids: row
                    .sub_agents()
                    .iter()
                    .map(|agent| agent.id.clone())
                    .collect(),
            })
        })
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.row_id
            .cmp(&right.row_id)
            .then_with(|| left.pane_id.cmp(&right.pane_id))
    });

    let mut groups = current
        .worktree_groups
        .iter()
        .map(|group| {
            let mut row_ids = group
                .rows
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>();
            row_ids.sort();
            let mut status_counts = group
                .status_counts
                .iter()
                .map(|count| StatusCountSig {
                    status: count.status.as_str().to_owned(),
                    count: count.count,
                })
                .collect::<Vec<_>>();
            status_counts.sort();
            GroupSig {
                key: group.key.clone(),
                kind: group.kind,
                row_ids,
                hidden_count: group.hidden_count,
                status_counts,
            }
        })
        .collect::<Vec<_>>();
    groups.sort_by(|left, right| left.key.cmp(&right.key));

    FrameSig {
        at_ms: now_ms,
        panes_produced_at_ms: current.panes_produced_at_ms,
        rows,
        groups,
        own_view: current.own_view.as_ref().map(|view| OwnViewSig {
            sibling_count: view.sibling_count,
            active_pane_id: view.active_pane_id.as_ref().map(ToString::to_string),
            working_pane_ids: view
                .working_pane_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        }),
        events: extract_events(event_store, now_ms),
        pulled_rows: last_pulled
            .worktree_groups
            .iter()
            .map(|group| group.rows.len())
            .sum(),
        pulled_panes_produced_at_ms: last_pulled.panes_produced_at_ms,
        gate_reject_streak,
        health_failure_streak,
    }
}

impl RosterSig {
    pub fn from_frame(sig: &FrameSig) -> Self {
        Self {
            panes_produced_at_ms: sig.panes_produced_at_ms,
            rows: sig
                .rows
                .iter()
                .map(|row| RosterRowSig {
                    row_id: row.row_id.clone(),
                    is_agent: row.is_agent,
                    pane_id: row.pane_id.clone(),
                    pane_pid: row.pane_pid,
                    pane_process_start: row.pane_process_start,
                })
                .collect(),
        }
    }
}

fn extract_events(event_store: &EventStore, now_ms: u64) -> EventsSig {
    let mut events = EventsSig::default();
    for event in event_store.active(now_ms) {
        match &event.event {
            SidebarEvent::PaneClosed { pane_id } => events.pane_closed.push(EventPaneSig {
                pane_id: pane_id.to_string(),
                sent_at_ms: event.sent_at_ms,
            }),
            SidebarEvent::PaneOpened { pane_id, .. } => events.pane_opened.push(EventPaneSig {
                pane_id: pane_id.to_string(),
                sent_at_ms: event.sent_at_ms,
            }),
            SidebarEvent::CommandChanged { .. }
            | SidebarEvent::FocusChanged { .. }
            | SidebarEvent::FocusStranded { .. }
            | SidebarEvent::PanesChanged
            | SidebarEvent::LedgerDelta { .. }
            | SidebarEvent::PaneFramePublished
            | SidebarEvent::Notify { .. }
            | SidebarEvent::Reload => {}
        }
    }
    events
}
