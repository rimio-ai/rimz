//! Sidebar frame-stream observer.
//!
//! Every renderer feeds the observer one compact signature per committed frame.
//! The pure detectors run in-process; the writer thread handles cooldown,
//! rotation, and elder-only real-world checks.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::SidebarSnapshot;

mod detect;
pub mod log;
mod sig;

pub use detect::Observer;
pub use sig::{
    EventsSig, FrameSig, GroupSig, OwnViewSig, RosterRowSig, RosterSig, RowSig, StatusCountSig,
    WatchedValues, extract_sig,
};

const EVIDENCE_LIMIT: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchedField {
    Status,
    ContextPct,
    TotalTokens,
    TodoDone,
    GroupKey,
    Model,
}

#[derive(Clone, Debug)]
pub enum ObserveMsg {
    Anomaly(Box<AnomalyDraft>),
    Roster(RosterSig),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateRejectInfo {
    pub frameless_incoming: bool,
    pub demoted_panes: Vec<String>,
    pub incoming_rows: usize,
}

impl GateRejectInfo {
    pub fn from_snapshots(prev: &SidebarSnapshot, incoming: &SidebarSnapshot) -> Self {
        let agentish_panes = prev
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .filter(|row| row.is_agent())
            .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.to_string()))
            .collect::<BTreeSet<_>>();
        let demoted_panes = incoming
            .worktree_groups
            .iter()
            .flat_map(|group| group.rows.iter())
            .filter(|row| row.is_process())
            .filter_map(|row| row.pane.as_ref().map(|pane| pane.pane_id.to_string()))
            .filter(|pane_id| agentish_panes.contains(pane_id))
            .take(EVIDENCE_LIMIT)
            .collect();
        Self {
            frameless_incoming: incoming.panes_produced_at_ms.is_none(),
            demoted_panes,
            incoming_rows: incoming
                .worktree_groups
                .iter()
                .map(|group| group.rows.len())
                .sum(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AnomalyDraft {
    pub at_ms: u64,
    pub kind: AnomalyKind,
    pub window_ms: Option<u64>,
    pub frame: FrameStamp,
    pub events_recent: EventsSig,
    pub gate_reject_streak: u32,
    pub health_failure_streak: u32,
    pub dropped_msgs: u32,
}

impl AnomalyDraft {
    pub fn from_sig(sig: &FrameSig, kind: AnomalyKind, window_ms: Option<u64>) -> Self {
        Self {
            at_ms: sig.at_ms,
            kind,
            window_ms,
            frame: FrameStamp::from_sig(sig),
            events_recent: sig.events.clone(),
            gate_reject_streak: sig.gate_reject_streak,
            health_failure_streak: sig.health_failure_streak,
            dropped_msgs: 0,
        }
    }

    pub fn from_roster(at_ms: u64, roster: &RosterSig, kind: AnomalyKind) -> Self {
        Self {
            at_ms,
            kind,
            window_ms: None,
            frame: FrameStamp::from_roster(roster),
            events_recent: EventsSig::default(),
            gate_reject_streak: 0,
            health_failure_streak: 0,
            dropped_msgs: 0,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ObserveRecord {
    pub at_ms: u64,
    pub workspace_id: String,
    pub session: String,
    pub instance: String,
    pub role: ObserveRole,
    #[serde(flatten)]
    pub anomaly: AnomalyKind,
    pub window_ms: Option<u64>,
    pub frame: FrameStamp,
    pub events_recent: EventsSig,
    pub gate_reject_streak: u32,
    pub health_failure_streak: u32,
    pub suppressed_since_last: u32,
    pub dropped_msgs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveRole {
    Elder,
    Consumer,
}

#[derive(Clone, Debug, Serialize)]
pub struct FrameStamp {
    pub produced_at_ms: Option<u64>,
    pub rows: usize,
    pub agents: usize,
    pub processes: usize,
    pub pulled_rows: Option<usize>,
    pub pulled_panes_produced_at_ms: Option<u64>,
}

impl FrameStamp {
    fn from_sig(sig: &FrameSig) -> Self {
        Self {
            produced_at_ms: sig.panes_produced_at_ms,
            rows: sig.rows.len(),
            agents: sig.rows.iter().filter(|row| row.is_agent).count(),
            processes: sig.rows.iter().filter(|row| !row.is_agent).count(),
            pulled_rows: Some(sig.pulled_rows),
            pulled_panes_produced_at_ms: sig.pulled_panes_produced_at_ms,
        }
    }

    fn from_roster(roster: &RosterSig) -> Self {
        Self {
            produced_at_ms: roster.panes_produced_at_ms,
            rows: roster.rows.len(),
            agents: roster.rows.iter().filter(|row| row.is_agent).count(),
            processes: roster.rows.iter().filter(|row| !row.is_agent).count(),
            pulled_rows: None,
            pulled_panes_produced_at_ms: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AnomalyKind {
    RosterFlap {
        rows_before: usize,
        empty_at_ms: u64,
        restored_at_ms: u64,
        rows_after: usize,
    },
    RowPresenceFlap {
        row_id: String,
        pane_id: Option<String>,
        gone_at_ms: u64,
        back_at_ms: u64,
    },
    ShortLivedRow {
        row_id: String,
        pane_id: Option<String>,
        group_key: String,
        born_at_ms: u64,
        gone_at_ms: u64,
    },
    ValueOscillation {
        row_id: String,
        field: WatchedField,
        from: String,
        via: String,
        span_ms: u64,
    },
    StatusChurn {
        row_id: String,
        transitions: usize,
        window_ms: u64,
    },
    DuplicateRowId {
        row_id: String,
        count: usize,
    },
    DuplicatePaneRows {
        pane_id: String,
        row_ids: Vec<String>,
    },
    StatusCountMismatch {
        group_key: String,
        declared: Vec<StatusCountSig>,
        tallied: Vec<StatusCountSig>,
    },
    OwnViewIncoherent {
        active_pane_id: String,
        working_count: usize,
    },
    SubagentTopLevelLeak {
        agent_id: String,
    },
    SubagentDoubleRender {
        id: String,
    },
    FramelessRows {
        rows: Vec<String>,
    },
    CardsExceedPanes {
        rows: usize,
        frame_panes: usize,
        frame_produced_at_ms: u64,
    },
    RowPaneMissingFromFrame {
        row_id: String,
        pane_id: String,
        frame_produced_at_ms: u64,
    },
    DeadPid {
        row_id: String,
        pid: u32,
        reason: String,
    },
    GateReject {
        streak: u32,
        frameless_incoming: bool,
        demoted_panes: Vec<String>,
        incoming_rows: usize,
    },
    HealthDegraded {
        streak: u32,
        reason: String,
    },
}

impl AnomalyKind {
    pub fn key(&self) -> &'static str {
        match self {
            Self::RosterFlap { .. } => "roster_flap",
            Self::RowPresenceFlap { .. } => "row_presence_flap",
            Self::ShortLivedRow { .. } => "short_lived_row",
            Self::ValueOscillation { .. } => "value_oscillation",
            Self::StatusChurn { .. } => "status_churn",
            Self::DuplicateRowId { .. } => "duplicate_row_id",
            Self::DuplicatePaneRows { .. } => "duplicate_pane_rows",
            Self::StatusCountMismatch { .. } => "status_count_mismatch",
            Self::OwnViewIncoherent { .. } => "own_view_incoherent",
            Self::SubagentTopLevelLeak { .. } => "subagent_top_level_leak",
            Self::SubagentDoubleRender { .. } => "subagent_double_render",
            Self::FramelessRows { .. } => "frameless_rows",
            Self::CardsExceedPanes { .. } => "cards_exceed_panes",
            Self::RowPaneMissingFromFrame { .. } => "row_pane_missing_from_frame",
            Self::DeadPid { .. } => "dead_pid",
            Self::GateReject { .. } => "gate_reject",
            Self::HealthDegraded { .. } => "health_degraded",
        }
    }
}

fn cap_vec<T>(values: impl IntoIterator<Item = T>) -> Vec<T> {
    values.into_iter().take(EVIDENCE_LIMIT).collect()
}
