use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::agents::ExtraCredits;
pub use crate::diag::record::{
    AggregateKey, EventPaneSig, EventsSig, PanelField, SpendPeriod, StatusCountSig, WindowField,
};
use crate::sidebar::event_store::EventStore;
use crate::store::snapshot::{
    RedeemForecast, RemoteControlBadge, SidebarSnapshot, SidebarWorktreeKind,
};
use crate::wakeup::events::SidebarEvent;

use super::WatchedField;

#[cfg(test)]
thread_local! {
    static EXTRACTIONS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(crate) fn take_extractions() -> (usize, usize) {
    EXTRACTIONS.with(|count| count.replace((0, 0)))
}

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
    pub agent_kind: Option<String>,
    pub pane_id: Option<String>,
    pub pane_pid: Option<u32>,
    pub pane_process_start: Option<jiff::Timestamp>,
    pub group_key: String,
    pub watched: WatchedValues,
    pub attention_status: Option<String>,
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
    /// The values `value_oscillation` watches. Status is left out: a short
    /// turn or an answered ask is a legitimate status blink, and
    /// `status_churn` owns status motion.
    pub fn fields(&self) -> Vec<(WatchedField, Option<String>)> {
        vec![
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
        let aggregates = aggregate_values(snapshot)
            .into_iter()
            .map(|(key, value)| (key.identity(), value))
            .collect();
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
pub(crate) struct OwnFrameSig {
    pub at_ms: u64,
    pub frame: crate::diag::record::FrameStamp,
    pub own_view: Option<OwnViewSig>,
    pub events: EventsSig,
    pub pane_ids: Vec<Option<String>>,
    pub gate_reject_streak: u32,
    pub health_failure_streak: u32,
}

impl OwnFrameSig {
    pub(crate) fn extract(
        current: &SidebarSnapshot,
        pulled: (usize, Option<u64>),
        events: &EventStore,
        gate_reject_streak: u32,
        health_failure_streak: u32,
        at_ms: u64,
    ) -> Self {
        #[cfg(test)]
        EXTRACTIONS.with(|count| {
            let (common, own) = count.get();
            count.set((common, own + 1));
        });
        let mut agents = 0;
        let pane_ids: Vec<_> = current
            .rows()
            .map(|row| {
                agents += usize::from(row.is_agent());
                row.pane.as_ref().map(|pane| pane.pane_id.to_string())
            })
            .collect();
        Self {
            at_ms,
            frame: crate::diag::record::FrameStamp {
                produced_at_ms: current.panes_produced_at_ms,
                rows: pane_ids.len(),
                agents,
                processes: pane_ids.len() - agents,
                pulled_rows: Some(pulled.0),
                pulled_panes_produced_at_ms: pulled.1,
            },
            own_view: current.own_view.as_ref().map(|view| OwnViewSig {
                sibling_count: view.sibling_count,
                focused_pane: current.focused_pane.as_ref().map(ToString::to_string),
                working_pane_ids: view
                    .working_pane_ids
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
            }),
            events: extract_events(events, at_ms),
            pane_ids,
            gate_reject_streak,
            health_failure_streak,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RosterSig {
    pub panes_produced_at_ms: Option<u64>,
    pub rows: Vec<RosterRowSig>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterRowSig {
    pub row_id: String,
    pub agent_kind: Option<String>,
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
    #[cfg(test)]
    EXTRACTIONS.with(|count| {
        let (common, own) = count.get();
        count.set((common + 1, own));
    });
    let mut rows = current
        .worktree_groups
        .iter()
        .flat_map(|group| {
            group.rows.iter().map(|row| RowSig {
                row_id: row.id.clone(),
                agent_kind: row.is_agent().then(|| row.name.clone()),
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
                attention_status: row
                    .attention_status()
                    .map(|status| status.as_str().to_owned()),
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
    let mut aggregates = aggregate_values(current)
        .into_iter()
        .map(|(key, committed)| {
            let identity = key.identity();
            let pulled = last_pulled.aggregates.get(&identity).cloned().flatten();
            (
                identity,
                AggregateSig {
                    key,
                    committed,
                    pulled,
                },
            )
        })
        .collect::<Vec<_>>();
    aggregates.sort_by(|left, right| left.0.cmp(&right.0));
    aggregates
        .into_iter()
        .map(|(_, aggregate)| aggregate)
        .collect()
}

/// Every figure the observer keys on one snapshot, with its value as the
/// record prints it. The committed and the pulled signature are both built
/// from this list, so a key or a format cannot exist on one side alone. An
/// unset field is an absent value, and money is integer cents.
///
/// `active_sessions` and a window's `observed_at` stay out: both move in
/// normal operation, where a flip is not a fault.
fn aggregate_values(snapshot: &SidebarSnapshot) -> Vec<(AggregateKey, Option<String>)> {
    let year = |tally: Option<&crate::SpendTally>| tally.map(|tally| cents(tally.year.usd));
    let mut values = vec![
        (
            AggregateKey::CockpitTally,
            year(snapshot.value_tally.as_ref()),
        ),
        (
            AggregateKey::WorkspaceTally,
            year(snapshot.workspace_value_tally.as_ref()),
        ),
    ];
    for panel in &snapshot.providers {
        let login = panel.login_key();
        let spending = panel.spending.as_ref();
        values.push((
            AggregateKey::ProviderSpend {
                login: login.clone(),
            },
            spending.map(|tally| cents(tally.year.usd)),
        ));
        for (period, usd) in [
            (
                SpendPeriod::Headline,
                spending.map(|tally| tally.headline.usd),
            ),
            (SpendPeriod::Week, spending.map(|tally| tally.week.usd)),
            (SpendPeriod::Month, spending.map(|tally| tally.month.usd)),
        ] {
            values.push((
                AggregateKey::ProviderSpendPeriod {
                    login: login.clone(),
                    period,
                },
                usd.map(cents),
            ));
        }
        for window in &panel.windows {
            let scope_id = || window.scope.as_ref().map(|scope| scope.id.clone());
            values.push((
                AggregateKey::ProviderMana {
                    login: login.clone(),
                    scope_id: scope_id(),
                    duration_mins: window.duration_mins,
                },
                window.used_percentage.map(|pct| pct.to_string()),
            ));
            for (field, value) in [
                (
                    WindowField::ResetsAt,
                    window.resets_at.map(|at| at.to_string()),
                ),
                (WindowField::Lifted, Some(window.lifted.to_string())),
            ] {
                values.push((
                    AggregateKey::ProviderManaField {
                        login: login.clone(),
                        scope_id: scope_id(),
                        duration_mins: window.duration_mins,
                        field,
                    },
                    value,
                ));
            }
        }
        for (field, value) in [
            (PanelField::Version, panel.version.clone()),
            (PanelField::Plan, panel.plan.clone()),
            (PanelField::Metered, Some(panel.metered.to_string())),
            (
                PanelField::RemoteControl,
                Some(
                    match panel.remote_control {
                        RemoteControlBadge::Hidden => "hidden",
                        RemoteControlBadge::Healthy => "healthy",
                        RemoteControlBadge::Down => "down",
                    }
                    .to_owned(),
                ),
            ),
            (
                PanelField::DayBudget,
                panel.day_budget.map(|budget| {
                    let parked = if budget.parked { "/parked" } else { "" };
                    format!(
                        "{}/{}{parked}",
                        cents(budget.spend_usd),
                        cents(budget.cap_usd)
                    )
                }),
            ),
            (
                PanelField::ExtraCredits,
                panel.extra_credits.as_ref().map(|credits| match credits {
                    ExtraCredits::Disabled => "disabled".to_owned(),
                    ExtraCredits::Known {
                        used_usd,
                        remaining_usd,
                        limit_usd,
                    } => [used_usd, remaining_usd, limit_usd]
                        .map(|usd| usd.map_or_else(|| "-".to_owned(), cents))
                        .join("/"),
                }),
            ),
            (
                PanelField::ResetCredits,
                panel.reset_credits.as_ref().map(|credits| {
                    credits.soonest_expiry.map_or_else(
                        || credits.count.to_string(),
                        |expiry| format!("{}@{expiry}", credits.count),
                    )
                }),
            ),
            (
                PanelField::RedeemForecast,
                panel.redeem_forecast.map(|forecast| {
                    match forecast {
                        RedeemForecast::Manual => "manual",
                        RedeemForecast::Armed => "armed",
                        RedeemForecast::Holding => "holding",
                    }
                    .to_owned()
                }),
            ),
        ] {
            values.push((
                AggregateKey::ProviderField {
                    login: login.clone(),
                    field,
                },
                value,
            ));
        }
    }
    values
}

fn cents(usd: f64) -> String {
    ((usd * 100.0).round() as i64).to_string()
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
                    agent_kind: row.agent_kind.clone(),
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

    fn spent(usd: f64) -> crate::SpendWindow {
        crate::SpendWindow {
            usd,
            ..Default::default()
        }
    }

    /// A panel with every keyed field set, in a snapshot whose cockpit and
    /// workspace tallies are set too.
    fn full_snapshot() -> SidebarSnapshot {
        let at = |secs| jiff::Timestamp::from_second(secs).unwrap();
        let tally = crate::SpendTally {
            headline: spent(1.5),
            week: spent(20.25),
            month: spent(300.0),
            year: spent(4000.01),
        };
        let panel = crate::store::snapshot::SidebarProviderPanel {
            version: Some("2.1.291".to_owned()),
            plan: Some("Claude Max".to_owned()),
            remote_control: crate::store::snapshot::RemoteControlBadge::Healthy,
            active_sessions: 3,
            spending: Some(tally.clone()),
            day_budget: Some(crate::store::snapshot::DailyBudgetView {
                cap_usd: 50.0,
                spend_usd: 12.5,
                parked: true,
            }),
            extra_credits: Some(crate::agents::ExtraCredits::Known {
                used_usd: Some(3.0),
                remaining_usd: None,
                limit_usd: Some(10.0),
            }),
            reset_credits: Some(crate::agents::ResetCredits {
                count: 2,
                soonest_expiry: Some(at(1_800_000_000)),
                expiries: vec![at(1_800_000_000)],
                effect: crate::agents::RedeemEffect::RestartsWindow,
            }),
            redeem_forecast: Some(crate::store::snapshot::RedeemForecast::Armed),
            ..provider_panel(
                "claude",
                vec![RateLimitWindow {
                    used_percentage: Some(40),
                    resets_at: Some(at(1_790_000_000)),
                    duration_mins: Some(300),
                    observed_at: Some(at(1_789_000_000)),
                    ..Default::default()
                }],
            )
        };
        let mut snapshot = snapshot_with_panels(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/sig-full")),
            vec![panel],
        );
        snapshot.value_tally = Some(tally.clone());
        snapshot.workspace_value_tally = Some(tally);
        snapshot
    }

    fn self_pulled(snapshot: &SidebarSnapshot) -> Vec<AggregateSig> {
        extract_aggregates(snapshot, &PulledFrameSig::from_snapshot(snapshot))
    }

    #[test]
    fn committed_and_pulled_signatures_agree_on_every_panel_key() {
        let aggregates = self_pulled(&full_snapshot());
        let values = aggregates
            .iter()
            .map(|aggregate| {
                assert_eq!(aggregate.pulled, aggregate.committed, "{:?}", aggregate.key);
                (aggregate.key.identity(), aggregate.pulled.clone())
            })
            .collect::<Vec<_>>();
        let expected = [
            ("cockpit_tally", "400001"),
            (
                "provider_field:claude@default:day_budget",
                "1250/5000/parked",
            ),
            ("provider_field:claude@default:extra_credits", "300/-/1000"),
            ("provider_field:claude@default:metered", "true"),
            ("provider_field:claude@default:plan", "Claude Max"),
            ("provider_field:claude@default:redeem_forecast", "armed"),
            ("provider_field:claude@default:remote_control", "healthy"),
            (
                "provider_field:claude@default:reset_credits",
                "2@2027-01-15T08:00:00Z",
            ),
            ("provider_field:claude@default:version", "2.1.291"),
            ("provider_mana:claude@default:300", "40"),
            ("provider_mana:claude@default:300:lifted", "false"),
            (
                "provider_mana:claude@default:300:resets_at",
                "2026-09-21T14:13:20Z",
            ),
            ("provider_spend:claude@default", "400001"),
            ("provider_spend:claude@default:headline", "150"),
            ("provider_spend:claude@default:month", "30000"),
            ("provider_spend:claude@default:week", "2025"),
            ("workspace_tally", "400001"),
        ]
        .map(|(identity, value)| (identity.to_owned(), Some(value.to_owned())));
        assert_eq!(values, expected);
    }

    #[test]
    fn unset_panel_fields_are_absent_values() {
        let snapshot = snapshot_with_panels(
            WorkspaceId::from_project_root(std::path::Path::new("/tmp/sig-unset")),
            vec![provider_panel("claude", Vec::new())],
        );
        let set = self_pulled(&snapshot)
            .into_iter()
            .filter(|aggregate| aggregate.committed.is_some())
            .map(|aggregate| (aggregate.key.identity(), aggregate.committed))
            .collect::<Vec<_>>();
        assert_eq!(
            set,
            [
                ("provider_field:claude@default:metered", "true"),
                ("provider_field:claude@default:remote_control", "hidden"),
            ]
            .map(|(identity, value)| (identity.to_owned(), Some(value.to_owned())))
        );
    }

    #[test]
    fn session_count_and_reading_time_are_not_keyed() {
        let steady = full_snapshot();
        let mut churned = steady.clone();
        churned.providers[0].active_sessions = 9;
        churned.providers[0].windows[0].observed_at =
            Some(jiff::Timestamp::from_second(1_789_000_500).unwrap());
        assert_eq!(self_pulled(&churned), self_pulled(&steady));
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
