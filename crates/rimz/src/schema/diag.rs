//! Durable sidebar diagnostic record schema.
//!
//! Diagnostics are evidence, not correctness input. Records are anomaly-only
//! JSONL entries under the workspace state directory.

use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId, PaneId, SidebarInstanceId, WorkspaceId};

pub const DIAG_SCHEMA_VERSION: &str = "rimz.diag.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagEnvelope {
    pub v: String,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<SidebarInstanceId>,
    pub at_ms: u64,
    pub severity: DiagSeverity,
    pub event: DiagEvent,
}

impl DiagEnvelope {
    pub fn new(
        workspace_id: WorkspaceId,
        session_name: String,
        instance_id: Option<SidebarInstanceId>,
        at_ms: u64,
        event: DiagEvent,
    ) -> Self {
        Self {
            v: DIAG_SCHEMA_VERSION.to_owned(),
            workspace_id,
            session_name,
            instance_id,
            at_ms,
            severity: event.severity(),
            event,
        }
    }

    pub fn is_current_version(&self) -> bool {
        self.v == DIAG_SCHEMA_VERSION
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DiagEvent {
    FrameRejected {
        reason: FrameRejectReason,
        prior_pane_count: usize,
        fresh_pane_count: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frames_ref: Option<String>,
    },
    FrameRejectEscape {
        held_ms: u64,
    },
    FrameShrinkVerified {
        prior: usize,
        fresh: usize,
    },
    PaneCountDrop {
        prior: usize,
        new: usize,
        removed: Vec<PaneId>,
        added: Vec<PaneId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frames_ref: Option<String>,
    },
    GateHold {
        rule: GateRule,
        prev_produced_at_ms: Option<u64>,
        incoming_produced_at_ms: Option<u64>,
        reject_streak: u32,
    },
    GateRelease {
        rule: GateRule,
        held_ms: u64,
        via_escape_hatch: bool,
    },
    FetchFailure {
        reason: String,
        failure_streak: u32,
    },
    HealthAlert {
        reason: String,
        since_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovered_after_ms: Option<u64>,
    },
    ProducerElected {
        prior_elder: SidebarInstanceId,
    },
    ProducerDemoted {
        new_elder: SidebarInstanceId,
    },
    RowConflict {
        agent_kind: AgentKind,
        agent_session_id: AgentSessionId,
        bound_pane: PaneId,
        conflicting_pane: PaneId,
    },
    DuplicatePaneId {
        pane_id: PaneId,
    },
    ForeignSessionPane {
        pane_id: PaneId,
        session: String,
    },
    GroupMigration {
        pane_id: PaneId,
        from: GroupIdentity,
        to: GroupIdentity,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_before: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd_after: Option<String>,
    },
    NewbornQuarantined {
        pane_id: PaneId,
    },
    RendererPanic {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backtrace: Option<String>,
    },
    FrameAnomaly {
        role: ObserveRole,
        anomaly: AnomalyKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_ms: Option<u64>,
        frame: FrameStamp,
        events_recent: EventsSig,
        gate_reject_streak: u32,
        health_failure_streak: u32,
        suppressed_since_last: u32,
        dropped_msgs: u32,
    },
}

impl DiagEvent {
    pub fn severity(&self) -> DiagSeverity {
        match self {
            Self::FrameRejected { .. }
            | Self::PaneCountDrop { .. }
            | Self::GateHold { .. }
            | Self::FetchFailure { .. }
            | Self::HealthAlert {
                recovered_after_ms: None,
                ..
            }
            | Self::RowConflict { .. }
            | Self::DuplicatePaneId { .. }
            | Self::ForeignSessionPane { .. } => DiagSeverity::Warn,
            Self::FrameAnomaly { .. } => DiagSeverity::Warn,
            Self::RendererPanic { .. } => DiagSeverity::Error,
            Self::FrameRejectEscape { .. }
            | Self::FrameShrinkVerified { .. }
            | Self::GateRelease { .. }
            | Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::GroupMigration { .. }
            | Self::NewbornQuarantined { .. }
            | Self::HealthAlert {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::FrameRejected { .. } => "frame_rejected",
            Self::FrameRejectEscape { .. } => "frame_reject_escape",
            Self::FrameShrinkVerified { .. } => "frame_shrink_verified",
            Self::PaneCountDrop { .. } => "pane_count_drop",
            Self::GateHold { .. } => "gate_hold",
            Self::GateRelease { .. } => "gate_release",
            Self::FetchFailure { .. } => "fetch_failure",
            Self::HealthAlert { .. } => "health_alert",
            Self::ProducerElected { .. } => "producer_elected",
            Self::ProducerDemoted { .. } => "producer_demoted",
            Self::RowConflict { .. } => "row_conflict",
            Self::DuplicatePaneId { .. } => "duplicate_pane_id",
            Self::ForeignSessionPane { .. } => "foreign_session_pane",
            Self::GroupMigration { .. } => "group_migration",
            Self::NewbornQuarantined { .. } => "newborn_quarantined",
            Self::RendererPanic { .. } => "renderer_panic",
            Self::FrameAnomaly { .. } => "frame_anomaly",
        }
    }

    pub fn identity_key(&self) -> String {
        match self {
            Self::FrameRejected { reason, .. } => format!("{}:{reason:?}", self.kind_name()),
            Self::PaneCountDrop { removed, added, .. } => {
                format!("{}:{removed:?}:{added:?}", self.kind_name())
            }
            Self::GateHold { rule, .. } | Self::GateRelease { rule, .. } => {
                format!("{}:{rule:?}", self.kind_name())
            }
            Self::FetchFailure { reason, .. } => format!("{}:{reason}", self.kind_name()),
            Self::HealthAlert {
                reason,
                since_ms,
                recovered_after_ms,
            } => {
                let phase = if recovered_after_ms.is_some() {
                    "recovered"
                } else {
                    "active"
                };
                format!("{}:{reason}:{phase}:{since_ms}", self.kind_name())
            }
            Self::RowConflict {
                agent_kind,
                agent_session_id,
                bound_pane,
                conflicting_pane,
            } => format!(
                "{}:{agent_kind}:{agent_session_id}:{bound_pane}:{conflicting_pane}",
                self.kind_name()
            ),
            Self::DuplicatePaneId { pane_id } | Self::NewbornQuarantined { pane_id } => {
                format!("{}:{pane_id}", self.kind_name())
            }
            Self::ForeignSessionPane { pane_id, session } => {
                format!("{}:{pane_id}:{session}", self.kind_name())
            }
            Self::GroupMigration {
                pane_id, from, to, ..
            } => {
                format!(
                    "{}:{pane_id}:{}:{}:{}:{}",
                    self.kind_name(),
                    from.kind,
                    from.key,
                    to.kind,
                    to.key
                )
            }
            Self::FrameAnomaly { anomaly, .. } => format!(
                "{}:{}:{}",
                self.kind_name(),
                anomaly.key(),
                anomaly.subject().unwrap_or_default()
            ),
            Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::FrameRejectEscape { .. }
            | Self::FrameShrinkVerified { .. }
            | Self::RendererPanic { .. } => self.kind_name().to_owned(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum FrameRejectReason {
    Empty,
    MissingOwnPane,
    MuxError { stderr_excerpt: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateRule {
    FramelessOverFrame,
    AgentDemotedToProcess,
    EmptyStampedFrame,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupIdentity {
    pub kind: String,
    pub key: String,
}

/// Whether the writing instance was the elected elder (runs real-world
/// cross-checks) or a plain consumer at record time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserveRole {
    Elder,
    Consumer,
}

/// Per-row value the frame-stream observer watches for oscillation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchedField {
    Status,
    ContextPct,
    TotalTokens,
    TodoDone,
    GroupKey,
    Model,
}

/// One status histogram bucket as a group declares (or a re-tally finds) it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StatusCountSig {
    pub status: String,
    pub count: usize,
}

/// Compact stamp of the committed frame a [`DiagEvent::FrameAnomaly`] was
/// judged on, plus the un-fused pulled-truth scalars so every record shows
/// whether the published frame already held the anomaly.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameStamp {
    pub produced_at_ms: Option<u64>,
    pub rows: usize,
    pub agents: usize,
    pub processes: usize,
    pub pulled_rows: Option<usize>,
    pub pulled_panes_produced_at_ms: Option<u64>,
}

/// Active pane open/close events at record time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsSig {
    pub pane_closed: Vec<EventPaneSig>,
    pub pane_opened: Vec<EventPaneSig>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPaneSig {
    pub pane_id: String,
    pub sent_at_ms: u64,
}

/// What the frame-stream observer judged anomalous; the detectors live in
/// [`crate::sidebar::observe`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "detector", rename_all = "snake_case")]
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
        }
    }

    /// The row/pane/group identity an anomaly is about, for rate-limit
    /// identity; detectors with whole-frame scope have no subject.
    pub fn subject(&self) -> Option<&str> {
        match self {
            Self::RowPresenceFlap { row_id, .. }
            | Self::ShortLivedRow { row_id, .. }
            | Self::ValueOscillation { row_id, .. }
            | Self::StatusChurn { row_id, .. }
            | Self::DuplicateRowId { row_id, .. }
            | Self::RowPaneMissingFromFrame { row_id, .. }
            | Self::DeadPid { row_id, .. } => Some(row_id),
            Self::DuplicatePaneRows { pane_id, .. } => Some(pane_id),
            Self::StatusCountMismatch { group_key, .. } => Some(group_key),
            Self::OwnViewIncoherent { active_pane_id, .. } => Some(active_pane_id),
            Self::SubagentTopLevelLeak { agent_id } => Some(agent_id),
            Self::SubagentDoubleRender { id } => Some(id),
            Self::RosterFlap { .. }
            | Self::FramelessRows { .. }
            | Self::CardsExceedPanes { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::MuxName;

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn sidebar(raw: &str) -> SidebarInstanceId {
        SidebarInstanceId::parse(raw).expect("valid sidebar instance id")
    }

    #[test]
    fn serde_round_trips_each_variant() {
        let events = vec![
            DiagEvent::FrameRejected {
                reason: FrameRejectReason::Empty,
                prior_pane_count: 2,
                fresh_pane_count: 0,
                frames_ref: Some("frame.1.0.frame_rejected.json".to_owned()),
            },
            DiagEvent::FrameRejectEscape { held_ms: 5_001 },
            DiagEvent::FrameShrinkVerified { prior: 8, fresh: 3 },
            DiagEvent::PaneCountDrop {
                prior: 3,
                new: 0,
                removed: vec![pane("terminal_1")],
                added: Vec::new(),
                frames_ref: Some("frame.1.pane_count_drop.json".to_owned()),
            },
            DiagEvent::GateHold {
                rule: GateRule::EmptyStampedFrame,
                prev_produced_at_ms: Some(1),
                incoming_produced_at_ms: Some(2),
                reject_streak: 1,
            },
            DiagEvent::GateRelease {
                rule: GateRule::AgentDemotedToProcess,
                held_ms: 1_000,
                via_escape_hatch: false,
            },
            DiagEvent::FetchFailure {
                reason: "pane discovery failed".to_owned(),
                failure_streak: 2,
            },
            DiagEvent::HealthAlert {
                reason: "snapshot failed".to_owned(),
                since_ms: 10,
                recovered_after_ms: Some(20),
            },
            DiagEvent::ProducerElected {
                prior_elder: sidebar("sb_019e8c565bbd708097fce9514f79da04"),
            },
            DiagEvent::ProducerDemoted {
                new_elder: sidebar("sb_019e8c565bbd7b22854f93a905e1034c"),
            },
            DiagEvent::RowConflict {
                agent_kind: AgentKind::new_unchecked("claude"),
                agent_session_id: AgentSessionId::from("sess-1"),
                bound_pane: pane("terminal_1"),
                conflicting_pane: pane("terminal_2"),
            },
            DiagEvent::DuplicatePaneId {
                pane_id: pane("terminal_1"),
            },
            DiagEvent::ForeignSessionPane {
                pane_id: pane("terminal_1"),
                session: "other".to_owned(),
            },
            DiagEvent::GroupMigration {
                pane_id: pane("terminal_1"),
                from: GroupIdentity {
                    kind: "external".to_owned(),
                    key: "external".to_owned(),
                },
                to: GroupIdentity {
                    kind: "worktree".to_owned(),
                    key: "/repo".to_owned(),
                },
                cwd_before: None,
                cwd_after: Some("/repo".to_owned()),
            },
            DiagEvent::NewbornQuarantined {
                pane_id: pane("terminal_1"),
            },
            DiagEvent::RendererPanic {
                message: "boom".to_owned(),
                backtrace: None,
            },
            DiagEvent::FrameAnomaly {
                role: ObserveRole::Elder,
                anomaly: AnomalyKind::RowPresenceFlap {
                    row_id: "agent-1".to_owned(),
                    pane_id: Some("zellij:terminal_1".to_owned()),
                    gone_at_ms: 11_000,
                    back_at_ms: 12_000,
                },
                window_ms: Some(7_000),
                frame: FrameStamp {
                    produced_at_ms: Some(12_000),
                    rows: 2,
                    agents: 2,
                    processes: 0,
                    pulled_rows: Some(2),
                    pulled_panes_produced_at_ms: Some(12_000),
                },
                events_recent: EventsSig::default(),
                gate_reject_streak: 0,
                health_failure_streak: 0,
                suppressed_since_last: 3,
                dropped_msgs: 0,
            },
        ];
        for event in events {
            let envelope = DiagEnvelope::new(
                WorkspaceId::from_project_root(std::path::Path::new("/repo")),
                "rimz-test".to_owned(),
                None,
                42,
                event,
            );
            let encoded = serde_json::to_vec(&envelope).expect("encode");
            let decoded: DiagEnvelope = serde_json::from_slice(&encoded).expect("decode");
            assert_eq!(decoded, envelope);
            assert!(decoded.is_current_version());
        }
    }

    #[test]
    fn health_alert_identity_distinguishes_phase_and_episode() {
        let active = DiagEvent::HealthAlert {
            reason: "snapshot failed".to_owned(),
            since_ms: 10,
            recovered_after_ms: None,
        };
        let recovered = DiagEvent::HealthAlert {
            reason: "snapshot failed".to_owned(),
            since_ms: 10,
            recovered_after_ms: Some(500),
        };
        let next_episode = DiagEvent::HealthAlert {
            reason: "snapshot failed".to_owned(),
            since_ms: 20,
            recovered_after_ms: None,
        };

        assert_ne!(active.identity_key(), recovered.identity_key());
        assert_ne!(active.identity_key(), next_episode.identity_key());
    }

    #[test]
    fn newborn_quarantine_is_informational() {
        assert_eq!(
            DiagEvent::NewbornQuarantined {
                pane_id: pane("terminal_1")
            }
            .severity(),
            DiagSeverity::Info
        );
    }
}
