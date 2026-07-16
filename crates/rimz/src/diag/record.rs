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
    /// Suppressed same-identity records since the previous emitted record, plus
    /// per-kind ceiling drops flushed onto this passing record.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererExitCause {
    SelfCloseEmptyTab,
    DegradedGaveUp,
}

impl RendererExitCause {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelfCloseEmptyTab => "self_close_empty_tab",
            Self::DegradedGaveUp => "degraded_gave_up",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostedCarryDropReason {
    ProbeReportsAbsent,
    StartRegressed,
    ForegroundKindMismatch,
    CarryExpired,
}

impl HostedCarryDropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProbeReportsAbsent => "probe_reports_absent",
            Self::StartRegressed => "start_regressed",
            Self::ForegroundKindMismatch => "foreground_kind_mismatch",
            Self::CarryExpired => "carry_expired",
        }
    }
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
    ResolutionFallback {
        reason: String,
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
    HostedCarryDropped {
        pane_id: PaneId,
        agent_kind: AgentKind,
        reason: HostedCarryDropReason,
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
    ClientReaped {
        killed_pids: Vec<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pre_clients: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        post_clients: Option<usize>,
        settled: bool,
        timed_out: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        errors: Vec<String>,
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
    TopologyWriterChanged {
        prior_plugin_id: u32,
        prior_loaded_at_ms: u64,
        plugin_id: u32,
        loaded_at_ms: u64,
    },
    TopologyWriteRejected {
        plugin_id: u32,
        loaded_at_ms: u64,
        accepted_plugin_id: u32,
        accepted_loaded_at_ms: u64,
        rejected_count: u64,
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
    RendererExit {
        cause: RendererExitCause,
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
            | Self::ClientReaped { settled: false, .. }
            | Self::TickBudgetBreach {
                recovered_after_ms: None,
                ..
            }
            | Self::TopologyWriteRejected { .. }
            | Self::RowConflict { .. }
            | Self::DuplicatePaneId { .. }
            | Self::ForeignSessionPane { .. } => DiagSeverity::Warn,
            Self::HostedCarryDropped {
                reason:
                    HostedCarryDropReason::StartRegressed
                    | HostedCarryDropReason::ForegroundKindMismatch,
                ..
            } => DiagSeverity::Warn,
            Self::FrameAnomaly { .. } => DiagSeverity::Warn,
            Self::RendererPanic { .. } => DiagSeverity::Error,
            Self::RendererSignalDeath { .. } => DiagSeverity::Error,
            Self::RendererExit {
                cause: RendererExitCause::DegradedGaveUp,
            } => DiagSeverity::Warn,
            Self::FrameShrinkVerified { .. }
            | Self::ResolutionFallback { .. }
            | Self::PaneCarryRefuted { .. }
            | Self::GateRelease { .. }
            | Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::HostedCarryDropped {
                reason:
                    HostedCarryDropReason::ProbeReportsAbsent | HostedCarryDropReason::CarryExpired,
                ..
            }
            | Self::GroupMigration { .. }
            | Self::NewbornQuarantined { .. }
            | Self::MixedBuildWriters { .. }
            | Self::TopologyWriterChanged { .. }
            | Self::RendererExit {
                cause: RendererExitCause::SelfCloseEmptyTab,
            }
            | Self::HealthAlert {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
            Self::LinkAlert {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
            Self::ClientReaped { settled: true, .. } => DiagSeverity::Info,
            Self::TickBudgetBreach {
                recovered_after_ms: Some(_),
                ..
            } => DiagSeverity::Info,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::FrameRejected { .. } => "frame_rejected",
            Self::ResolutionFallback { .. } => "resolution_fallback",
            Self::FrameShrinkVerified { .. } => "frame_shrink_verified",
            Self::PaneCountDrop { .. } => "pane_count_drop",
            Self::PaneCarryForward { .. } => "pane_carry_forward",
            Self::PaneCarryRefuted { .. } => "pane_carry_refuted",
            Self::CarryForwardExpired { .. } => "carry_forward_expired",
            Self::HostedCarryDropped { .. } => "hosted_carry_dropped",
            Self::GateHold { .. } => "gate_hold",
            Self::GateRelease { .. } => "gate_release",
            Self::FetchFailure { .. } => "fetch_failure",
            Self::HealthAlert { .. } => "health_alert",
            Self::LinkAlert { .. } => "link_alert",
            Self::ClientReaped { .. } => "client_reaped",
            Self::TickBudgetBreach { .. } => "tick_budget_breach",
            Self::ProducerElected { .. } => "producer_elected",
            Self::ProducerDemoted { .. } => "producer_demoted",
            Self::RowConflict { .. } => "row_conflict",
            Self::DuplicatePaneId { .. } => "duplicate_pane_id",
            Self::ForeignSessionPane { .. } => "foreign_session_pane",
            Self::GroupMigration { .. } => "group_migration",
            Self::NewbornQuarantined { .. } => "newborn_quarantined",
            Self::MixedBuildWriters { .. } => "mixed_build_writers",
            Self::TopologyWriterChanged { .. } => "topology_writer_changed",
            Self::TopologyWriteRejected { .. } => "topology_write_rejected",
            Self::RendererPanic { .. } => "renderer_panic",
            Self::RendererSignalDeath { .. } => "renderer_signal_death",
            Self::RendererExit { .. } => "renderer_exit",
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
            Self::HostedCarryDropped {
                pane_id,
                agent_kind,
                reason,
            } => {
                format!(
                    "{}:{agent_kind}:{pane_id}:{}",
                    self.kind_name(),
                    reason.as_str()
                )
            }
            Self::GateHold { rule, .. } | Self::GateRelease { rule, .. } => {
                format!("{}:{rule:?}", self.kind_name())
            }
            Self::FetchFailure { reason, .. } => format!("{}:{reason}", self.kind_name()),
            Self::ResolutionFallback { reason } => format!("{}:{reason}", self.kind_name()),
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
            Self::ClientReaped {
                killed_pids,
                settled,
                ..
            } => format!("{}:{killed_pids:?}:{settled}", self.kind_name()),
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
            Self::TopologyWriterChanged {
                prior_plugin_id,
                prior_loaded_at_ms,
                plugin_id,
                loaded_at_ms,
            } => format!(
                "{}:{prior_loaded_at_ms}:{prior_plugin_id}->{loaded_at_ms}:{plugin_id}",
                self.kind_name()
            ),
            Self::TopologyWriteRejected {
                plugin_id,
                loaded_at_ms,
                accepted_plugin_id,
                accepted_loaded_at_ms,
                ..
            } => format!(
                "{}:{loaded_at_ms}:{plugin_id}->{accepted_loaded_at_ms}:{accepted_plugin_id}",
                self.kind_name()
            ),
            Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::FrameShrinkVerified { .. }
            | Self::RendererPanic { .. } => self.kind_name().to_owned(),
            Self::RendererSignalDeath {
                signal, exit_code, ..
            } => {
                format!("{}:{signal:?}:{exit_code:?}", self.kind_name())
            }
            Self::RendererExit { cause } => format!("{}:{}", self.kind_name(), cause.as_str()),
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope_id: Option<String>,
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
                scope_id,
                duration_mins,
            } => {
                if let Some(scope_id) = scope_id {
                    return format!("provider_mana:{kind}:scope:{scope_id}");
                }
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
    MultiFocusTopology {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tab_position: Option<u64>,
        pane_ids: Vec<String>,
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
            Self::MultiFocusTopology { .. } => "multi_focus_topology",
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
            Self::MultiFocusTopology {
                tab_position: Some(tab_position),
                ..
            } => Some(Cow::Owned(tab_position.to_string())),
            Self::MultiFocusTopology {
                tab_name: Some(tab_name),
                ..
            } => Some(Cow::Borrowed(tab_name)),
            Self::MultiFocusTopology { pane_ids, .. } => {
                pane_ids.first().map(|pane| Cow::Borrowed(pane.as_str()))
            }
            Self::RosterFlap { .. }
            | Self::FramelessRows { .. }
            | Self::CardsExceedPanes { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests;
