use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

pub use crate::diag::record::{AggregateKey, EventPaneSig, EventsSig, StatusCountSig};
use crate::sidebar::events::{EventStore, SidebarEvent};
use crate::store::snapshot::{SidebarSnapshot, SidebarWorktreeKind};

use super::WatchedField;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameSig {
    pub at_ms: u64,
    pub panes_produced_at_ms: Option<u64>,
    pub rows: Vec<RowSig>,
    pub groups: Vec<GroupSig>,
    pub aggregates: Vec<AggregateSig>,
    pub own_view: Option<OwnViewSig>,
    pub events: EventsSig,
    pub pulled_rows: usize,
    pub pulled_panes_produced_at_ms: Option<u64>,
    pub pulled_row_ids: BTreeSet<String>,
    pub pulled_pane_ids: BTreeSet<String>,
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
    pub render_order: Vec<String>,
    pub status_counts: Vec<StatusCountSig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AggregateSig {
    pub key: AggregateKey,
    pub committed: Option<String>,
    pub pulled: Option<String>,
}

/// The small pulled-truth subset diagnostics compares with a committed frame.
/// Keeping this projection lets the renderer move an overlay-free pull into
/// presentation without retaining and cloning the full snapshot.
#[derive(Clone, Debug)]
pub struct PulledFrameSig {
    rows: usize,
    panes_produced_at_ms: Option<u64>,
    row_ids: BTreeSet<String>,
    pane_ids: BTreeSet<String>,
    aggregates: BTreeMap<String, Option<String>>,
}

impl PulledFrameSig {
    pub fn from_snapshot(snapshot: &SidebarSnapshot) -> Self {
        let mut aggregates = BTreeMap::new();
        aggregates.insert(
            AggregateKey::CockpitTally.identity(),
            spend_cents(snapshot.value_tally.as_ref()),
        );
        aggregates.insert(
            AggregateKey::WorkspaceTally.identity(),
            spend_cents(snapshot.workspace_value_tally.as_ref()),
        );
        for panel in &snapshot.providers {
            aggregates.insert(
                AggregateKey::ProviderSpend {
                    kind: panel.kind.clone(),
                }
                .identity(),
                spend_cents(panel.spending.as_ref()),
            );
            for window in &panel.windows {
                aggregates.insert(
                    AggregateKey::ProviderMana {
                        kind: panel.kind.clone(),
                        scope_id: window.scope.as_ref().map(|scope| scope.id.clone()),
                        duration_mins: window.duration_mins,
                    }
                    .identity(),
                    window.used_percentage.map(|pct| pct.to_string()),
                );
            }
        }
        Self {
            rows: snapshot
                .worktree_groups
                .iter()
                .map(|group| group.rows.len())
                .sum(),
            panes_produced_at_ms: snapshot.panes_produced_at_ms,
            row_ids: snapshot.rows().map(|row| row.id.clone()).collect(),
            pane_ids: snapshot
                .rows()
                .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.to_string()))
                .collect(),
            aggregates,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnViewSig {
    pub sibling_count: usize,
    pub focused_pane: Option<String>,
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
    last_pulled: &PulledFrameSig,
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
            let render_order = group
                .rows
                .iter()
                .map(|row| row.id.clone())
                .collect::<Vec<_>>();
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
                render_order,
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
        aggregates: extract_aggregates(current, last_pulled),
        own_view: current.own_view.as_ref().map(|view| OwnViewSig {
            sibling_count: view.sibling_count,
            focused_pane: current.focused_pane.as_ref().map(ToString::to_string),
            working_pane_ids: view
                .working_pane_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        }),
        events: extract_events(event_store, now_ms),
        pulled_rows: last_pulled.rows,
        pulled_panes_produced_at_ms: last_pulled.panes_produced_at_ms,
        pulled_row_ids: last_pulled.row_ids.clone(),
        pulled_pane_ids: last_pulled.pane_ids.clone(),
        gate_reject_streak,
        health_failure_streak,
    }
}

fn extract_aggregates(
    current: &SidebarSnapshot,
    last_pulled: &PulledFrameSig,
) -> Vec<AggregateSig> {
    let mut aggregates = vec![
        AggregateSig {
            key: AggregateKey::CockpitTally,
            committed: spend_cents(current.value_tally.as_ref()),
            pulled: last_pulled
                .aggregates
                .get(&AggregateKey::CockpitTally.identity())
                .cloned()
                .flatten(),
        },
        AggregateSig {
            key: AggregateKey::WorkspaceTally,
            committed: spend_cents(current.workspace_value_tally.as_ref()),
            pulled: last_pulled
                .aggregates
                .get(&AggregateKey::WorkspaceTally.identity())
                .cloned()
                .flatten(),
        },
    ];
    for panel in &current.providers {
        let spend_key = AggregateKey::ProviderSpend {
            kind: panel.kind.clone(),
        };
        aggregates.push(AggregateSig {
            key: spend_key.clone(),
            committed: spend_cents(panel.spending.as_ref()),
            pulled: last_pulled
                .aggregates
                .get(&spend_key.identity())
                .cloned()
                .flatten(),
        });
        for window in &panel.windows {
            let key = AggregateKey::ProviderMana {
                kind: panel.kind.clone(),
                scope_id: window.scope.as_ref().map(|scope| scope.id.clone()),
                duration_mins: window.duration_mins,
            };
            aggregates.push(AggregateSig {
                key: key.clone(),
                committed: window.used_percentage.map(|pct| pct.to_string()),
                pulled: last_pulled
                    .aggregates
                    .get(&key.identity())
                    .cloned()
                    .flatten(),
            });
        }
    }
    aggregates.sort_by_key(|aggregate| aggregate.key.identity());
    aggregates
}

fn spend_cents(tally: Option<&crate::SpendTally>) -> Option<String> {
    tally.map(|tally| format!("{}", (tally.year.usd * 100.0).round() as i64))
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
            | SidebarEvent::FocusIntent { .. }
            | SidebarEvent::PanesChanged
            | SidebarEvent::StoreDelta { .. }
            | SidebarEvent::WidthTargetChanged
            | SidebarEvent::BodyFilterChanged
            | SidebarEvent::PaneFramePublished { .. }
            | SidebarEvent::Notify { .. }
            | SidebarEvent::Reload => {}
        }
    }
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{RateLimitWindow, RateLimitWindowScope};
    use crate::ids::WorkspaceId;
    use crate::sidebar::test_support::{provider_panel, snapshot_with_panels};

    fn scoped(id: &str, used: u8) -> RateLimitWindow {
        RateLimitWindow {
            scope: Some(RateLimitWindowScope {
                id: id.to_owned(),
                label: id[..3].to_owned(),
            }),
            used_percentage: Some(used),
            ..Default::default()
        }
    }

    #[test]
    fn pulled_named_quota_lookup_uses_scope_identity() {
        let workspace = WorkspaceId::from_project_root(std::path::Path::new("/tmp/sig-scopes"));
        let current = snapshot_with_panels(
            workspace.clone(),
            vec![provider_panel(
                "copilot",
                vec![scoped("premium", 20), scoped("chat", 70)],
            )],
        );
        let pulled = snapshot_with_panels(
            workspace,
            vec![provider_panel(
                "copilot",
                vec![scoped("chat", 30), scoped("premium", 60)],
            )],
        );
        let mana = extract_aggregates(&current, &PulledFrameSig::from_snapshot(&pulled))
            .into_iter()
            .filter(|aggregate| matches!(aggregate.key, AggregateKey::ProviderMana { .. }))
            .collect::<Vec<_>>();
        assert_eq!(mana.len(), 2);
        let premium = mana
            .iter()
            .find(|aggregate| aggregate.key.identity().ends_with(":premium"))
            .unwrap();
        assert_eq!(premium.committed.as_deref(), Some("20"));
        assert_eq!(premium.pulled.as_deref(), Some("60"));
        let chat = mana
            .iter()
            .find(|aggregate| aggregate.key.identity().ends_with(":chat"))
            .unwrap();
        assert_eq!(chat.committed.as_deref(), Some("70"));
        assert_eq!(chat.pulled.as_deref(), Some("30"));
    }
}
