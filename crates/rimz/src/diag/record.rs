//! Durable sidebar diagnostic record schema.
//!
//! Diagnostics are evidence, not correctness input. Records are anomaly-only
//! JSONL entries under the workspace state directory.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId, PaneId, SidebarInstanceId, WorkspaceId};
use crate::remote::link::LinkTier;

pub const DIAG_SCHEMA_VERSION: &str = "rimz.diag.v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagEnvelope {
    pub v: String,
    /// Build id of the writing process ([`crate::build_id`]), so overlapping
    /// old/new builds are distinguishable in the evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub workspace_id: WorkspaceId,
    pub session_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<SidebarInstanceId>,
    pub at_ms: u64,
    pub severity: DiagSeverity,
    pub event: DiagEvent,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub suppressed_since_last: u32,
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
            build: crate::build_id::current().map(str::to_owned),
            workspace_id,
            session_name,
            instance_id,
            at_ms,
            severity: event.severity(),
            event,
            suppressed_since_last: 0,
        }
    }

    pub fn with_suppressed(mut self, suppressed_since_last: u32) -> Self {
        self.suppressed_since_last = suppressed_since_last;
        self
    }

    pub fn is_current_version(&self) -> bool {
        self.v == DIAG_SCHEMA_VERSION
    }
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagSeverity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TickLoop {
    Fetch,
    CacheRefresh,
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
    PaneCarryForward {
        carried: Vec<PaneId>,
        pids: Vec<u32>,
        prior: usize,
        fresh: usize,
        cli_confirmed: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frames_ref: Option<String>,
    },
    PaneCarryRefuted {
        carried: Vec<PaneId>,
        pids: Vec<u32>,
        prior: usize,
        fresh: usize,
        verified: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frames_ref: Option<String>,
    },
    CarryForwardExpired {
        pane_id: PaneId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
        carried_ms: u64,
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
    LinkAlert {
        tier: LinkTier,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rtt_ms: Option<u32>,
        miss_pct: u16,
        since_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recovered_after_ms: Option<u64>,
    },
    TickBudgetBreach {
        tick_loop: TickLoop,
        /// Consecutive over-budget ticks at emit time.
        over_ticks: u32,
        /// Values from the tick that emitted this record.
        #[serde(default)]
        last_wall_ms: u64,
        #[serde(default)]
        last_mux_wait_ms: u64,
        #[serde(default)]
        last_fold_bytes: u64,
        #[serde(default)]
        last_spawns: u64,
        /// Worst values observed in the streak.
        wall_ms: u64,
        #[serde(default)]
        mux_wait_ms: u64,
        fold_bytes: u64,
        spawns: u64,
        /// The declared bounds the sample was judged against.
        budget_wall_ms: u64,
        #[serde(default)]
        budget_mux_wait_ms: u64,
        budget_fold_bytes: u64,
        budget_spawns: u64,
        /// Unix ms of the streak's first over-budget tick.
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
    MixedBuildWriters {
        prior_build: String,
        own_build: String,
    },
    RendererPanic {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        backtrace: Option<String>,
    },
    RendererSignalDeath {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signal: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        stderr_excerpt: String,
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
            | Self::PaneCarryForward { .. }
            | Self::CarryForwardExpired { .. }
            | Self::GateHold { .. }
            | Self::FetchFailure { .. }
            | Self::HealthAlert {
                recovered_after_ms: None,
                ..
            }
            | Self::LinkAlert {
                recovered_after_ms: None,
                ..
            }
            | Self::TickBudgetBreach {
                recovered_after_ms: None,
                ..
            }
            | Self::RowConflict { .. }
            | Self::DuplicatePaneId { .. }
            | Self::ForeignSessionPane { .. } => DiagSeverity::Warn,
            Self::FrameAnomaly { .. } => DiagSeverity::Warn,
            Self::RendererPanic { .. } => DiagSeverity::Error,
            Self::RendererSignalDeath { .. } => DiagSeverity::Error,
            Self::FrameShrinkVerified { .. }
            | Self::PaneCarryRefuted { .. }
            | Self::GateRelease { .. }
            | Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::GroupMigration { .. }
            | Self::NewbornQuarantined { .. }
            | Self::MixedBuildWriters { .. }
            | Self::HealthAlert {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
            Self::LinkAlert {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
            Self::TickBudgetBreach {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::FrameRejected { .. } => "frame_rejected",
            Self::FrameShrinkVerified { .. } => "frame_shrink_verified",
            Self::PaneCountDrop { .. } => "pane_count_drop",
            Self::PaneCarryForward { .. } => "pane_carry_forward",
            Self::PaneCarryRefuted { .. } => "pane_carry_refuted",
            Self::CarryForwardExpired { .. } => "carry_forward_expired",
            Self::GateHold { .. } => "gate_hold",
            Self::GateRelease { .. } => "gate_release",
            Self::FetchFailure { .. } => "fetch_failure",
            Self::HealthAlert { .. } => "health_alert",
            Self::LinkAlert { .. } => "link_alert",
            Self::TickBudgetBreach { .. } => "tick_budget_breach",
            Self::ProducerElected { .. } => "producer_elected",
            Self::ProducerDemoted { .. } => "producer_demoted",
            Self::RowConflict { .. } => "row_conflict",
            Self::DuplicatePaneId { .. } => "duplicate_pane_id",
            Self::ForeignSessionPane { .. } => "foreign_session_pane",
            Self::GroupMigration { .. } => "group_migration",
            Self::NewbornQuarantined { .. } => "newborn_quarantined",
            Self::MixedBuildWriters { .. } => "mixed_build_writers",
            Self::RendererPanic { .. } => "renderer_panic",
            Self::RendererSignalDeath { .. } => "renderer_signal_death",
            Self::FrameAnomaly { .. } => "frame_anomaly",
        }
    }

    pub fn identity_key(&self) -> String {
        match self {
            Self::FrameRejected { reason, .. } => format!("{}:{reason:?}", self.kind_name()),
            Self::PaneCountDrop { removed, added, .. } => {
                format!("{}:{removed:?}:{added:?}", self.kind_name())
            }
            Self::PaneCarryForward { carried, .. } => {
                format!("{}:{carried:?}", self.kind_name())
            }
            Self::PaneCarryRefuted { carried, .. } => {
                format!("{}:{carried:?}", self.kind_name())
            }
            Self::CarryForwardExpired { pane_id, .. } => {
                format!("{}:{pane_id}", self.kind_name())
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
            Self::LinkAlert {
                tier,
                since_ms,
                recovered_after_ms,
                ..
            } => {
                let phase = if recovered_after_ms.is_some() {
                    "recovered"
                } else {
                    "active"
                };
                format!("{}:{tier:?}:{phase}:{since_ms}", self.kind_name())
            }
            Self::TickBudgetBreach {
                tick_loop,
                since_ms,
                recovered_after_ms,
                ..
            } => {
                let phase = if recovered_after_ms.is_some() {
                    "recovered"
                } else {
                    "active"
                };
                format!("{}:{tick_loop:?}:{phase}:{since_ms}", self.kind_name())
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
            Self::MixedBuildWriters {
                prior_build,
                own_build,
            } => format!("{}:{prior_build}:{own_build}", self.kind_name()),
            Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::FrameShrinkVerified { .. }
            | Self::RendererPanic { .. } => self.kind_name().to_owned(),
            Self::RendererSignalDeath {
                signal, exit_code, ..
            } => {
                format!("{}:{signal:?}:{exit_code:?}", self.kind_name())
            }
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "aggregate", rename_all = "snake_case")]
pub enum AggregateKey {
    CockpitTally,
    WorkspaceTally,
    ProviderSpend {
        kind: String,
    },
    ProviderMana {
        kind: String,
        duration_mins: Option<u32>,
    },
}

impl AggregateKey {
    pub fn identity(&self) -> String {
        match self {
            Self::CockpitTally => "cockpit_tally".to_owned(),
            Self::WorkspaceTally => "workspace_tally".to_owned(),
            Self::ProviderSpend { kind } => format!("provider_spend:{kind}"),
            Self::ProviderMana {
                kind,
                duration_mins,
            } => {
                let duration = duration_mins
                    .map(|mins| mins.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                format!("provider_mana:{kind}:{duration}")
            }
        }
    }

    /// True for monetary spend tallies, whose trailing-year figure never
    /// drops to zero in place. Provider mana windows roll to zero normally.
    pub fn is_spend_tally(&self) -> bool {
        matches!(
            self,
            Self::CockpitTally | Self::WorkspaceTally | Self::ProviderSpend { .. }
        )
    }
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
    AggregateOscillation {
        aggregate: AggregateKey,
        from: String,
        via: String,
        back: String,
        span_ms: u64,
        pulled_via: Option<String>,
    },
    AggregateReset {
        aggregate: AggregateKey,
        from: String,
        pulled: Option<String>,
    },
    OrderFlap {
        group_key: String,
        order: Vec<String>,
        via_order: Vec<String>,
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
            Self::AggregateOscillation { .. } => "aggregate_oscillation",
            Self::AggregateReset { .. } => "aggregate_reset",
            Self::OrderFlap { .. } => "order_flap",
            Self::StatusChurn { .. } => "status_churn",
            Self::DuplicateRowId { .. } => "duplicate_row_id",
            Self::DuplicatePaneRows { .. } => "duplicate_pane_rows",
            Self::StatusCountMismatch { .. } => "status_count_mismatch",
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
    pub fn subject(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::RowPresenceFlap { row_id, .. }
            | Self::ShortLivedRow { row_id, .. }
            | Self::ValueOscillation { row_id, .. }
            | Self::StatusChurn { row_id, .. }
            | Self::DuplicateRowId { row_id, .. }
            | Self::RowPaneMissingFromFrame { row_id, .. }
            | Self::DeadPid { row_id, .. } => Some(Cow::Borrowed(row_id)),
            Self::DuplicatePaneRows { pane_id, .. } => Some(Cow::Borrowed(pane_id)),
            Self::StatusCountMismatch { group_key, .. } | Self::OrderFlap { group_key, .. } => {
                Some(Cow::Borrowed(group_key))
            }
            Self::SubagentTopLevelLeak { agent_id } => Some(Cow::Borrowed(agent_id)),
            Self::SubagentDoubleRender { id } => Some(Cow::Borrowed(id)),
            Self::AggregateOscillation { aggregate, .. }
            | Self::AggregateReset { aggregate, .. } => Some(Cow::Owned(aggregate.identity())),
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

    fn workspace_id() -> WorkspaceId {
        WorkspaceId::from_project_root(std::path::Path::new("/repo"))
    }

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn sidebar(raw: &str) -> SidebarInstanceId {
        SidebarInstanceId::parse(raw).expect("valid sidebar instance id")
    }

    fn frame_rejected(frames_ref: Option<&str>) -> DiagEvent {
        DiagEvent::FrameRejected {
            reason: FrameRejectReason::Empty,
            prior_pane_count: 2,
            fresh_pane_count: 0,
            frames_ref: frames_ref.map(str::to_owned),
        }
    }

    fn frame_stamp(produced_at_ms: u64) -> FrameStamp {
        FrameStamp {
            produced_at_ms: Some(produced_at_ms),
            rows: 2,
            agents: 2,
            processes: 0,
            pulled_rows: Some(2),
            pulled_panes_produced_at_ms: Some(produced_at_ms),
        }
    }

    fn frame_anomaly(anomaly: AnomalyKind) -> DiagEvent {
        DiagEvent::FrameAnomaly {
            role: ObserveRole::Consumer,
            anomaly,
            window_ms: None,
            frame: frame_stamp(13_000),
            events_recent: EventsSig::default(),
            gate_reject_streak: 0,
            health_failure_streak: 0,
            suppressed_since_last: 0,
            dropped_msgs: 0,
        }
    }

    fn tick_budget_breach(
        tick_loop: TickLoop,
        since_ms: u64,
        recovered_after_ms: Option<u64>,
    ) -> DiagEvent {
        DiagEvent::TickBudgetBreach {
            tick_loop,
            over_ticks: 5,
            last_wall_ms: 1_200,
            last_mux_wait_ms: 0,
            last_fold_bytes: 300_000,
            last_spawns: 2,
            wall_ms: 1_200,
            mux_wait_ms: 0,
            fold_bytes: 300_000,
            spawns: 2,
            budget_wall_ms: 1_000,
            budget_mux_wait_ms: 5_000,
            budget_fold_bytes: 262_144,
            budget_spawns: 32,
            since_ms,
            recovered_after_ms,
        }
    }

    #[test]
    fn new_envelopes_pin_schema_build_severity_suppression_and_legacy_build() {
        let envelope = DiagEnvelope::new(
            workspace_id(),
            "rimz-test".to_owned(),
            Some(sidebar("sb_019e8c565bbd708097fce9514f79da04")),
            42,
            frame_rejected(None),
        );

        assert_eq!(envelope.v, DIAG_SCHEMA_VERSION);
        assert_eq!(envelope.build.as_deref(), crate::build_id::current());
        assert!(envelope.build.is_some());
        assert_eq!(envelope.severity, DiagSeverity::Warn);
        assert_eq!(envelope.suppressed_since_last, 0);
        assert!(envelope.is_current_version());

        let value = serde_json::to_value(&envelope).expect("encode");
        assert_eq!(value["severity"], "warn");
        assert!(value.get("suppressed_since_last").is_none());

        let suppressed = serde_json::to_value(envelope.clone().with_suppressed(2)).expect("encode");
        assert_eq!(suppressed["suppressed_since_last"], 2);

        let mut legacy = value;
        legacy.as_object_mut().expect("object").remove("build");
        let decoded: DiagEnvelope = serde_json::from_value(legacy).expect("decode");

        assert_eq!(decoded.build, None);
        assert!(decoded.is_current_version());
    }

    #[test]
    fn tick_budget_breach_deserializes_legacy_records_without_last_sample() {
        let value = serde_json::json!({
            "kind": "tick_budget_breach",
            "tick_loop": "fetch",
            "over_ticks": 5,
            "wall_ms": 1_200,
            "fold_bytes": 300_000,
            "spawns": 2,
            "budget_wall_ms": 1_000,
            "budget_fold_bytes": 262_144,
            "budget_spawns": 32,
            "since_ms": 10
        });

        let decoded: DiagEvent = serde_json::from_value(value).expect("decode legacy breach");

        assert_eq!(
            decoded,
            DiagEvent::TickBudgetBreach {
                tick_loop: TickLoop::Fetch,
                over_ticks: 5,
                last_wall_ms: 0,
                last_mux_wait_ms: 0,
                last_fold_bytes: 0,
                last_spawns: 0,
                wall_ms: 1_200,
                mux_wait_ms: 0,
                fold_bytes: 300_000,
                spawns: 2,
                budget_wall_ms: 1_000,
                budget_mux_wait_ms: 0,
                budget_fold_bytes: 262_144,
                budget_spawns: 32,
                since_ms: 10,
                recovered_after_ms: None,
            }
        );
    }

    #[test]
    fn severity_table_pins_product_categories() {
        let rows = [
            (frame_rejected(None), DiagSeverity::Warn),
            (
                DiagEvent::HealthAlert {
                    reason: "snapshot failed".to_owned(),
                    since_ms: 10,
                    recovered_after_ms: None,
                },
                DiagSeverity::Warn,
            ),
            (
                DiagEvent::LinkAlert {
                    tier: LinkTier::Degraded,
                    rtt_ms: Some(230),
                    miss_pct: 4,
                    since_ms: 10,
                    recovered_after_ms: None,
                },
                DiagSeverity::Warn,
            ),
            (
                tick_budget_breach(TickLoop::Fetch, 10, None),
                DiagSeverity::Warn,
            ),
            (
                DiagEvent::HealthAlert {
                    reason: "snapshot failed".to_owned(),
                    since_ms: 10,
                    recovered_after_ms: Some(20),
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::LinkAlert {
                    tier: LinkTier::Good,
                    rtt_ms: Some(42),
                    miss_pct: 0,
                    since_ms: 10,
                    recovered_after_ms: Some(40_000),
                },
                DiagSeverity::Info,
            ),
            (
                tick_budget_breach(TickLoop::CacheRefresh, 20, Some(8_000)),
                DiagSeverity::Info,
            ),
            (
                DiagEvent::PaneCarryRefuted {
                    carried: vec![pane("terminal_1")],
                    pids: vec![42],
                    prior: 2,
                    fresh: 1,
                    verified: 2,
                    frames_ref: None,
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::NewbornQuarantined {
                    pane_id: pane("terminal_1"),
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::ProducerElected {
                    prior_elder: sidebar("sb_019e8c565bbd708097fce9514f79da04"),
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::ProducerDemoted {
                    new_elder: sidebar("sb_019e8c565bbd7b22854f93a905e1034c"),
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::MixedBuildWriters {
                    prior_build: "0f3a9c21d4be".to_owned(),
                    own_build: "8e7d6c5b4a39".to_owned(),
                },
                DiagSeverity::Info,
            ),
            (
                DiagEvent::RendererPanic {
                    message: "boom".to_owned(),
                    backtrace: None,
                },
                DiagSeverity::Error,
            ),
            (
                DiagEvent::RendererSignalDeath {
                    signal: Some(6),
                    exit_code: None,
                    stderr_excerpt: "memory allocation failed".to_owned(),
                },
                DiagSeverity::Error,
            ),
        ];

        for (event, severity) in rows {
            assert_eq!(event.severity(), severity, "{event:?}");
        }
    }

    #[test]
    fn identity_key_table_pins_phase_episode_loop_and_subjects() {
        let rows = [
            (
                DiagEvent::HealthAlert {
                    reason: "snapshot failed".to_owned(),
                    since_ms: 10,
                    recovered_after_ms: None,
                },
                "health_alert:snapshot failed:active:10",
            ),
            (
                DiagEvent::HealthAlert {
                    reason: "snapshot failed".to_owned(),
                    since_ms: 10,
                    recovered_after_ms: Some(500),
                },
                "health_alert:snapshot failed:recovered:10",
            ),
            (
                DiagEvent::HealthAlert {
                    reason: "snapshot failed".to_owned(),
                    since_ms: 20,
                    recovered_after_ms: None,
                },
                "health_alert:snapshot failed:active:20",
            ),
            (
                DiagEvent::LinkAlert {
                    tier: LinkTier::Bad,
                    rtt_ms: Some(800),
                    miss_pct: 40,
                    since_ms: 10,
                    recovered_after_ms: None,
                },
                "link_alert:Bad:active:10",
            ),
            (
                DiagEvent::LinkAlert {
                    tier: LinkTier::Good,
                    rtt_ms: Some(42),
                    miss_pct: 0,
                    since_ms: 10,
                    recovered_after_ms: Some(40_000),
                },
                "link_alert:Good:recovered:10",
            ),
            (
                tick_budget_breach(TickLoop::Fetch, 10, None),
                "tick_budget_breach:Fetch:active:10",
            ),
            (
                tick_budget_breach(TickLoop::Fetch, 10, Some(500)),
                "tick_budget_breach:Fetch:recovered:10",
            ),
            (
                tick_budget_breach(TickLoop::CacheRefresh, 10, None),
                "tick_budget_breach:CacheRefresh:active:10",
            ),
            (
                DiagEvent::DuplicatePaneId {
                    pane_id: pane("terminal_1"),
                },
                "duplicate_pane_id:zellij:terminal_1",
            ),
            (
                DiagEvent::DuplicatePaneId {
                    pane_id: pane("terminal_2"),
                },
                "duplicate_pane_id:zellij:terminal_2",
            ),
            (
                DiagEvent::RowConflict {
                    agent_kind: AgentKind::new_unchecked("claude"),
                    agent_session_id: AgentSessionId::from("sess-1"),
                    bound_pane: pane("terminal_1"),
                    conflicting_pane: pane("terminal_2"),
                },
                "row_conflict:claude:sess-1:zellij:terminal_1:zellij:terminal_2",
            ),
            (
                DiagEvent::MixedBuildWriters {
                    prior_build: "0f3a9c21d4be".to_owned(),
                    own_build: "8e7d6c5b4a39".to_owned(),
                },
                "mixed_build_writers:0f3a9c21d4be:8e7d6c5b4a39",
            ),
            (
                DiagEvent::RendererSignalDeath {
                    signal: Some(6),
                    exit_code: None,
                    stderr_excerpt: "memory allocation failed".to_owned(),
                },
                "renderer_signal_death:Some(6):None",
            ),
        ];

        for (event, identity) in rows {
            assert_eq!(event.identity_key(), identity, "{event:?}");
        }
    }

    #[test]
    fn frame_anomaly_schema_and_identity_pin_detector_subjects() {
        let row = frame_anomaly(AnomalyKind::RowPresenceFlap {
            row_id: "agent-1".to_owned(),
            pane_id: Some("zellij:terminal_1".to_owned()),
            gone_at_ms: 11_000,
            back_at_ms: 12_000,
        });
        let aggregate = frame_anomaly(AnomalyKind::AggregateReset {
            aggregate: AggregateKey::ProviderSpend {
                kind: "claude".to_owned(),
            },
            from: "1234".to_owned(),
            pulled: Some("0".to_owned()),
        });

        assert_eq!(
            row.identity_key(),
            "frame_anomaly:row_presence_flap:agent-1"
        );
        assert_eq!(
            aggregate.identity_key(),
            "frame_anomaly:aggregate_reset:provider_spend:claude"
        );

        let value = serde_json::to_value(&aggregate).expect("encode");
        assert_eq!(value["kind"], "frame_anomaly");
        assert_eq!(value["anomaly"]["detector"], "aggregate_reset");
        assert_eq!(value["anomaly"]["aggregate"]["aggregate"], "provider_spend");
        assert_eq!(value["anomaly"]["aggregate"]["kind"], "claude");
    }

    #[test]
    fn representative_events_keep_json_wire_shape() {
        let rows = [
            (
                serde_json::json!({
                    "kind": "frame_rejected",
                    "reason": { "reason": "empty" },
                    "prior_pane_count": 2,
                    "fresh_pane_count": 0,
                    "frames_ref": "frame.1.0.frame_rejected.json"
                }),
                frame_rejected(Some("frame.1.0.frame_rejected.json")),
            ),
            (
                serde_json::json!({
                    "kind": "link_alert",
                    "tier": "bad",
                    "rtt_ms": 800,
                    "miss_pct": 40,
                    "since_ms": 10
                }),
                DiagEvent::LinkAlert {
                    tier: LinkTier::Bad,
                    rtt_ms: Some(800),
                    miss_pct: 40,
                    since_ms: 10,
                    recovered_after_ms: None,
                },
            ),
            (
                serde_json::json!({
                    "kind": "link_alert",
                    "tier": "good",
                    "rtt_ms": 42,
                    "miss_pct": 0,
                    "since_ms": 10,
                    "recovered_after_ms": 40_000
                }),
                DiagEvent::LinkAlert {
                    tier: LinkTier::Good,
                    rtt_ms: Some(42),
                    miss_pct: 0,
                    since_ms: 10,
                    recovered_after_ms: Some(40_000),
                },
            ),
            (
                serde_json::json!({
                    "kind": "frame_anomaly",
                    "role": "consumer",
                    "anomaly": {
                        "detector": "aggregate_oscillation",
                        "aggregate": {
                            "aggregate": "provider_spend",
                            "kind": "claude"
                        },
                        "from": "1234",
                        "via": "0",
                        "back": "1234",
                        "span_ms": 7_000,
                        "pulled_via": "0"
                    },
                    "frame": {
                        "produced_at_ms": 13_000,
                        "rows": 2,
                        "agents": 2,
                        "processes": 0,
                        "pulled_rows": 2,
                        "pulled_panes_produced_at_ms": 13_000
                    },
                    "events_recent": {
                        "pane_closed": [],
                        "pane_opened": []
                    },
                    "gate_reject_streak": 0,
                    "health_failure_streak": 0,
                    "suppressed_since_last": 3,
                    "dropped_msgs": 0
                }),
                DiagEvent::FrameAnomaly {
                    role: ObserveRole::Consumer,
                    anomaly: AnomalyKind::AggregateOscillation {
                        aggregate: AggregateKey::ProviderSpend {
                            kind: "claude".to_owned(),
                        },
                        from: "1234".to_owned(),
                        via: "0".to_owned(),
                        back: "1234".to_owned(),
                        span_ms: 7_000,
                        pulled_via: Some("0".to_owned()),
                    },
                    window_ms: None,
                    frame: frame_stamp(13_000),
                    events_recent: EventsSig::default(),
                    gate_reject_streak: 0,
                    health_failure_streak: 0,
                    suppressed_since_last: 3,
                    dropped_msgs: 0,
                },
            ),
        ];

        for (value, expected) in rows {
            let decoded: DiagEvent = serde_json::from_value(value.clone()).expect("decode");

            assert_eq!(decoded, expected);
            assert_eq!(serde_json::to_value(&decoded).expect("encode"), value);
        }
    }
}
