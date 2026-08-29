//! Durable sidebar diagnostic record schema.
//!
//! Diagnostics are evidence, not correctness input. Records are anomaly-only
//! JSONL entries under the workspace state directory.

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use crate::ids::{AgentKind, AgentSessionId, PaneId, SidebarInstanceId, ViewId, WorkspaceId};
use crate::remote::link::LinkTier;

const DIAG_SCHEMA_VERSION: &str = "rimz.diag.v1";

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

    pub(super) fn is_current_version(&self) -> bool {
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
pub enum FetchFoldCause {
    StoreDelta,
    Topology,
    Metrics,
    Presence,
    Backstop,
    WatchTransition,
    HardRefresh,
    Recovery,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFoldCauseStats {
    pub cause: FetchFoldCause,
    pub memo_skips: u64,
    pub full_folds: u64,
    pub adoptions: u64,
    pub fallbacks: u64,
    pub fold_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RendererExitCause {
    SelfCloseEmptyTab,
    DegradedGaveUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarWidthIntentTrigger {
    Narrower,
    Wider,
    MouseAdopt,
}

impl SidebarWidthIntentTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Narrower => "narrower",
            Self::Wider => "wider",
            Self::MouseAdopt => "mouse-adopt",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarWidthIntentVerdict {
    Accepted,
    RejectedFloor,
    RejectedFullscreen,
    RejectedNoStep,
}

impl SidebarWidthIntentVerdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::RejectedFloor => "rejected-floor",
            Self::RejectedFullscreen => "rejected-fullscreen",
            Self::RejectedNoStep => "rejected-no-step",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarWidthControlTrigger {
    Retarget,
    ResizeFeedback,
    Structural,
    Backstop,
    IdleRetry,
    Classification,
}

impl SidebarWidthControlTrigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Retarget => "retarget",
            Self::ResizeFeedback => "resize-feedback",
            Self::Structural => "structural",
            Self::Backstop => "backstop",
            Self::IdleRetry => "idle-retry",
            Self::Classification => "classification",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SidebarWidthSettleOutcome {
    FeedbackLearned,
    ReachedTolerance,
    ReverseParked,
    NoProgress,
    StepBudget,
}

impl SidebarWidthSettleOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::FeedbackLearned => "feedback-learned",
            Self::ReachedTolerance => "reached-tolerance",
            Self::ReverseParked => "reverse-parked",
            Self::NoProgress => "no-progress",
            Self::StepBudget => "step-budget",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkPaneBoundaryMove {
    pub pane: PaneId,
    pub from_x: u64,
    pub from_cols: u64,
    pub to_x: u64,
    pub to_cols: u64,
}

impl RendererExitCause {
    fn as_str(self) -> &'static str {
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
    fn as_str(self) -> &'static str {
        match self {
            Self::ProbeReportsAbsent => "probe_reports_absent",
            Self::StartRegressed => "start_regressed",
            Self::ForegroundKindMismatch => "foreground_kind_mismatch",
            Self::CarryExpired => "carry_expired",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalSessionBindRejectReason {
    StaleLaunchClock,
    PaneReserved,
    NoEvidence,
}

impl LocalSessionBindRejectReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::StaleLaunchClock => "stale_launch_clock",
            Self::PaneReserved => "pane_reserved",
            Self::NoEvidence => "no_evidence",
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
        evidence: Option<PaneDropEvidence>,
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
    SidebarWidthIntent {
        trigger: SidebarWidthIntentTrigger,
        own_cols: u16,
        base_cols: u16,
        #[serde(default)]
        view_cols: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step_cols: Option<u16>,
        step_exact: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_cols: Option<u16>,
        verdict: SidebarWidthIntentVerdict,
    },
    SidebarWidthNudge {
        trigger: SidebarWidthControlTrigger,
        #[serde(default)]
        view_cols: u16,
        from_cols: u16,
        target_cols: u16,
    },
    SidebarWidthSettle {
        settled_cols: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        learned_step: Option<u16>,
        outcome: SidebarWidthSettleOutcome,
    },
    WorkPaneBoundaryMoved {
        view_id: ViewId,
        view_cols: u64,
        moves: Vec<WorkPaneBoundaryMove>,
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
    FetchFoldStats {
        interval_ms: u64,
        causes: Vec<FetchFoldCauseStats>,
    },
    ToolLoopEscalated {
        agent_kind: AgentKind,
        agent_id: AgentSessionId,
        tool: String,
        count: u32,
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
    LocalSessionBindRejected {
        agent_kind: AgentKind,
        agent_session_id: AgentSessionId,
        pane_id: PaneId,
        reason: LocalSessionBindRejectReason,
    },
    GhostSessionBind {
        agent_kind: AgentKind,
        agent_session_id: AgentSessionId,
        pane_id: PaneId,
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
    RendererOrphanReaped {
        pane_id: String,
        worker_pid: i32,
    },
    SidebarOrphanReaped {
        pane_id: String,
        pid: i32,
        first_confirmed_at_ms: u64,
        second_confirmed_at_ms: u64,
        sigkilled: bool,
    },
    SubagentOrphanReaped {
        agent_kind: AgentKind,
        agent_id: AgentSessionId,
        parent_agent_id: AgentSessionId,
        orphaned_at_ms: u64,
    },
    SubagentOrphanRepairFailed {
        agent_kind: AgentKind,
        agent_id: AgentSessionId,
        parent_agent_id: AgentSessionId,
        orphaned_at_ms: u64,
        error: String,
    },
    PaneCacheDivergence {
        pane_id: String,
        pid: i32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_observed_at_ms: Option<u64>,
        authoritative_observed_at_ms: u64,
    },
    SupervisorConvergence {
        target_build: String,
    },
    SupervisorPreflightRejected {
        target_build: String,
        reason: String,
    },
    SelfCloseRejected {
        siblings: usize,
        reason: String,
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
            | Self::ToolLoopEscalated { .. }
            | Self::TopologyWriteRejected { .. }
            | Self::RendererOrphanReaped { .. }
            | Self::SidebarOrphanReaped { .. }
            | Self::SubagentOrphanReaped { .. }
            | Self::SubagentOrphanRepairFailed { .. }
            | Self::PaneCacheDivergence { .. }
            | Self::SupervisorPreflightRejected { .. }
            | Self::SelfCloseRejected { .. }
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
            Self::GhostSessionBind { .. } => DiagSeverity::Error,
            Self::RendererExit {
                cause: RendererExitCause::DegradedGaveUp,
            } => DiagSeverity::Warn,
            Self::FrameShrinkVerified { .. }
            | Self::ResolutionFallback { .. }
            | Self::SidebarWidthIntent { .. }
            | Self::SidebarWidthNudge { .. }
            | Self::SidebarWidthSettle { .. }
            | Self::WorkPaneBoundaryMoved { .. }
            | Self::FetchFoldStats { .. }
            | Self::PaneCarryRefuted { .. }
            | Self::GateRelease { .. }
            | Self::ProducerElected { .. }
            | Self::ProducerDemoted { .. }
            | Self::HostedCarryDropped {
                reason:
                    HostedCarryDropReason::ProbeReportsAbsent | HostedCarryDropReason::CarryExpired,
                ..
            }
            | Self::LocalSessionBindRejected { .. }
            | Self::GroupMigration { .. }
            | Self::NewbornQuarantined { .. }
            | Self::MixedBuildWriters { .. }
            | Self::TopologyWriterChanged { .. }
            | Self::SupervisorConvergence { .. }
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
            Self::SidebarWidthIntent { .. } => "sidebar_width_intent",
            Self::SidebarWidthNudge { .. } => "sidebar_width_nudge",
            Self::SidebarWidthSettle { .. } => "sidebar_width_settle",
            Self::WorkPaneBoundaryMoved { .. } => "work_pane_boundary_moved",
            Self::TickBudgetBreach { .. } => "tick_budget_breach",
            Self::FetchFoldStats { .. } => "fetch_fold_stats",
            Self::ToolLoopEscalated { .. } => "tool_loop_escalated",
            Self::ProducerElected { .. } => "producer_elected",
            Self::ProducerDemoted { .. } => "producer_demoted",
            Self::RowConflict { .. } => "row_conflict",
            Self::DuplicatePaneId { .. } => "duplicate_pane_id",
            Self::ForeignSessionPane { .. } => "foreign_session_pane",
            Self::LocalSessionBindRejected { .. } => "local_session_bind_rejected",
            Self::GhostSessionBind { .. } => "ghost_session_bind",
            Self::GroupMigration { .. } => "group_migration",
            Self::NewbornQuarantined { .. } => "newborn_quarantined",
            Self::MixedBuildWriters { .. } => "mixed_build_writers",
            Self::TopologyWriterChanged { .. } => "topology_writer_changed",
            Self::TopologyWriteRejected { .. } => "topology_write_rejected",
            Self::RendererPanic { .. } => "renderer_panic",
            Self::RendererSignalDeath { .. } => "renderer_signal_death",
            Self::RendererOrphanReaped { .. } => "renderer_orphan_reaped",
            Self::SidebarOrphanReaped { .. } => "sidebar_orphan_reaped",
            Self::SubagentOrphanReaped { .. } => "subagent_orphan_reaped",
            Self::SubagentOrphanRepairFailed { .. } => "subagent_orphan_repair_failed",
            Self::PaneCacheDivergence { .. } => "pane_cache_divergence",
            Self::SupervisorConvergence { .. } => "supervisor_convergence",
            Self::SupervisorPreflightRejected { .. } => "supervisor_preflight_rejected",
            Self::SelfCloseRejected { .. } => "self_close_rejected",
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
            Self::SidebarWidthIntent {
                trigger,
                base_cols,
                target_cols,
                verdict,
                ..
            } => format!(
                "{}:{}:{base_cols}:{target_cols:?}:{}",
                self.kind_name(),
                trigger.as_str(),
                verdict.as_str()
            ),
            Self::SidebarWidthNudge {
                trigger,
                from_cols,
                target_cols,
                ..
            } => format!(
                "{}:{}:{from_cols}:{target_cols}",
                self.kind_name(),
                trigger.as_str()
            ),
            Self::SidebarWidthSettle {
                settled_cols,
                learned_step,
                outcome,
            } => format!(
                "{}:{settled_cols}:{learned_step:?}:{}",
                self.kind_name(),
                outcome.as_str()
            ),
            Self::WorkPaneBoundaryMoved { view_id, .. } => {
                format!("{}:{view_id}", self.kind_name())
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
            Self::FetchFoldStats { .. } => self.kind_name().to_owned(),
            Self::ToolLoopEscalated {
                agent_kind,
                agent_id,
                tool,
                ..
            } => format!("{}:{agent_kind}:{agent_id}:{tool}", self.kind_name()),
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
            Self::LocalSessionBindRejected {
                agent_kind,
                agent_session_id,
                pane_id,
                reason,
            } => format!(
                "{}:{agent_kind}:{agent_session_id}:{pane_id}:{}",
                self.kind_name(),
                reason.as_str()
            ),
            Self::GhostSessionBind {
                agent_kind,
                agent_session_id,
                pane_id,
            } => format!(
                "{}:{agent_kind}:{agent_session_id}:{pane_id}",
                self.kind_name()
            ),
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
            Self::RendererOrphanReaped { pane_id, .. } => {
                format!("{}:{pane_id}", self.kind_name())
            }
            Self::SidebarOrphanReaped { pane_id, pid, .. }
            | Self::PaneCacheDivergence { pane_id, pid, .. } => {
                format!("{}:{pane_id}:{pid}", self.kind_name())
            }
            Self::SubagentOrphanReaped {
                agent_kind,
                agent_id,
                ..
            }
            | Self::SubagentOrphanRepairFailed {
                agent_kind,
                agent_id,
                ..
            } => format!("{}:{agent_kind}:{agent_id}", self.kind_name()),
            Self::SupervisorConvergence { target_build }
            | Self::SupervisorPreflightRejected { target_build, .. } => {
                format!("{}:{target_build}", self.kind_name())
            }
            Self::SelfCloseRejected { reason, .. } => {
                format!("{}:{reason}", self.kind_name())
            }
            Self::RendererExit { cause } => format!("{}:{}", self.kind_name(), cause.as_str()),
        }
    }

    /// The incident family this event belongs to: sibling variants that
    /// describe one ongoing incident share a family so the doctor groups them.
    pub fn family(&self) -> &'static str {
        match self {
            Self::GateHold { .. } | Self::GateRelease { .. } => "gate",
            Self::PaneCarryForward { .. } | Self::PaneCarryRefuted { .. } => "pane_carry",
            Self::FrameRejected { .. } | Self::FrameShrinkVerified { .. } => "frame_shrink",
            _ => self.kind_name(),
        }
    }

    /// The identity of the incident this event belongs to. Sibling variants
    /// within a family collapse onto one key so repeats fold into one row.
    pub fn family_key(&self) -> String {
        match self {
            Self::GateHold { rule, .. } | Self::GateRelease { rule, .. } => {
                format!("gate:{rule:?}")
            }
            Self::PaneCarryForward { carried, .. } | Self::PaneCarryRefuted { carried, .. } => {
                format!("pane_carry:{carried:?}")
            }
            Self::FrameRejected { .. } | Self::FrameShrinkVerified { .. } => {
                "frame_shrink".to_owned()
            }
            Self::HealthAlert {
                reason, since_ms, ..
            } => format!("health:{reason}:{since_ms}"),
            Self::LinkAlert { since_ms, .. } => format!("link:{since_ms}"),
            Self::TickBudgetBreach {
                tick_loop,
                since_ms,
                ..
            } => format!("tick:{tick_loop:?}:{since_ms}"),
            Self::TopologyWriteRejected {
                accepted_plugin_id,
                accepted_loaded_at_ms,
                ..
            }
            | Self::TopologyWriterChanged {
                plugin_id: accepted_plugin_id,
                loaded_at_ms: accepted_loaded_at_ms,
                ..
            } => format!("topology_writer:{accepted_loaded_at_ms}:{accepted_plugin_id}"),
            _ => self.identity_key(),
        }
    }

    /// Captured frame dumps this event points at, for follow-up inspection.
    pub fn evidence_refs(&self) -> Vec<String> {
        match self {
            Self::FrameRejected { frames_ref, .. }
            | Self::PaneCountDrop { frames_ref, .. }
            | Self::PaneCarryForward { frames_ref, .. }
            | Self::PaneCarryRefuted { frames_ref, .. } => frames_ref.iter().cloned().collect(),
            _ => Vec::new(),
        }
    }

    /// A one-line human description of what this event records.
    pub fn summary(&self) -> String {
        match self {
            Self::FrameRejected {
                reason,
                prior_pane_count,
                fresh_pane_count,
                frames_ref,
            } => format!(
                "rejected {reason:?}; panes {prior_pane_count}->{fresh_pane_count}{}",
                frames_ref
                    .as_ref()
                    .map(|name| format!("; frames {name}"))
                    .unwrap_or_default()
            ),
            Self::ResolutionFallback { reason } => {
                format!("resolution snapshot fell back to rollup: {reason}")
            }
            Self::FrameShrinkVerified { prior, fresh } => {
                format!("verified shrink {prior}->{fresh}")
            }
            Self::PaneCountDrop {
                prior,
                new,
                frames_ref,
                ..
            } => format!(
                "pane count {prior}->{new}{}",
                frames_ref
                    .as_ref()
                    .map(|name| format!("; frames {name}"))
                    .unwrap_or_default()
            ),
            Self::PaneCarryForward {
                carried,
                prior,
                fresh,
                cli_confirmed,
                frames_ref,
                ..
            } => format!(
                "carried {} panes over source shrink {prior}->{fresh}; cli_confirmed={cli_confirmed}{}",
                carried.len(),
                frames_ref
                    .as_ref()
                    .map(|name| format!("; frames {name}"))
                    .unwrap_or_default()
            ),
            Self::PaneCarryRefuted {
                carried,
                prior,
                fresh,
                verified,
                frames_ref,
                ..
            } => format!(
                "refuted {} carried panes after source re-pull {prior}->{fresh}->{verified}{}",
                carried.len(),
                frames_ref
                    .as_ref()
                    .map(|name| format!("; frames {name}"))
                    .unwrap_or_default()
            ),
            Self::CarryForwardExpired {
                pane_id,
                pid,
                carried_ms,
            } => match pid {
                Some(pid) => format!("expired carried {pane_id} pid {pid} after {carried_ms}ms"),
                None => format!("expired carried {pane_id} after {carried_ms}ms"),
            },
            Self::HostedCarryDropped {
                pane_id,
                agent_kind,
                reason,
            } => format!(
                "dropped hosted {agent_kind} carry for {pane_id}: {}",
                reason.as_str()
            ),
            Self::TopologyWriterChanged {
                prior_plugin_id,
                prior_loaded_at_ms,
                plugin_id,
                loaded_at_ms,
            } => format!(
                "topology writer changed {prior_loaded_at_ms}:{prior_plugin_id}->{loaded_at_ms}:{plugin_id}"
            ),
            Self::TopologyWriteRejected {
                plugin_id,
                loaded_at_ms,
                accepted_plugin_id,
                accepted_loaded_at_ms,
                rejected_count,
            } => format!(
                "rejected topology writer {loaded_at_ms}:{plugin_id}; accepted {accepted_loaded_at_ms}:{accepted_plugin_id}; count {rejected_count}"
            ),
            Self::GateHold {
                rule,
                reject_streak,
                ..
            } => format!("held {rule:?}; streak {reject_streak}"),
            Self::GateRelease {
                rule,
                held_ms,
                via_escape_hatch,
            } => format!("released {rule:?} after {held_ms}ms; escape={via_escape_hatch}"),
            Self::FetchFailure {
                reason,
                failure_streak,
            } => format!("{reason}; streak {failure_streak}"),
            Self::HealthAlert {
                reason,
                recovered_after_ms,
                ..
            } => match recovered_after_ms {
                Some(ms) => format!("recovered after {ms}ms: {reason}"),
                None => reason.clone(),
            },
            Self::LinkAlert {
                tier,
                rtt_ms,
                miss_pct,
                recovered_after_ms,
                ..
            } => {
                let rtt = rtt_ms
                    .map(|ms| format!("{ms}ms"))
                    .unwrap_or_else(|| "?".to_owned());
                match recovered_after_ms {
                    Some(ms) => format!("link recovered after {ms}ms; rtt {rtt}; loss {miss_pct}%"),
                    None => format!("link {tier:?}; rtt {rtt}; loss {miss_pct}%"),
                }
            }
            Self::ClientReaped {
                killed_pids,
                pre_clients,
                post_clients,
                settled,
                timed_out,
                errors,
            } => format!(
                "remote Zellij client reap pids {killed_pids:?}; clients {pre_clients:?}->{post_clients:?}; settled={settled}; timed_out={timed_out}{}",
                if errors.is_empty() {
                    String::new()
                } else {
                    format!("; {}", errors.join("; "))
                }
            ),
            Self::SidebarWidthIntent {
                trigger,
                own_cols,
                base_cols,
                view_cols,
                step_cols,
                step_exact,
                target_cols,
                verdict,
            } => format!(
                "sidebar width {}: own {own_cols}, base {base_cols}, view {view_cols}, step {step_cols:?} (exact={step_exact}), target {target_cols:?}; {}",
                trigger.as_str(),
                verdict.as_str(),
            ),
            Self::SidebarWidthNudge {
                trigger,
                view_cols,
                from_cols,
                target_cols,
            } => format!(
                "sidebar width nudge ({}) {from_cols}->{target_cols} in {view_cols} cols",
                trigger.as_str()
            ),
            Self::SidebarWidthSettle {
                settled_cols,
                learned_step,
                outcome,
            } => format!(
                "sidebar width settled at {settled_cols}; learned step {learned_step:?}; {}",
                outcome.as_str()
            ),
            Self::WorkPaneBoundaryMoved {
                view_id,
                view_cols,
                moves,
            } => {
                format!("work pane boundary moved in view {view_id} at {view_cols} cols: {moves:?}")
            }
            Self::TickBudgetBreach {
                tick_loop,
                over_ticks,
                last_wall_ms,
                last_mux_wait_ms,
                last_fold_bytes,
                last_spawns,
                wall_ms,
                mux_wait_ms,
                fold_bytes,
                spawns,
                budget_wall_ms,
                budget_mux_wait_ms,
                budget_fold_bytes,
                budget_spawns,
                recovered_after_ms,
                ..
            } => {
                let last = format!(
                    "last {last_wall_ms}ms ({last_mux_wait_ms}ms mux)/{last_fold_bytes}B/{last_spawns} spawns"
                );
                let worst = format!(
                    "worst {wall_ms}ms ({mux_wait_ms}ms mux)/{fold_bytes}B/{spawns} spawns"
                );
                let budget = format!(
                    "budget {budget_wall_ms}ms in-process/{budget_mux_wait_ms}ms mux/{budget_fold_bytes}B/{budget_spawns} spawns"
                );
                match recovered_after_ms {
                    Some(ms) => {
                        format!(
                            "{tick_loop:?} tick recovered after {ms}ms; {over_ticks} over ticks; {last}; {worst}; {budget}"
                        )
                    }
                    None => {
                        format!(
                            "{tick_loop:?} tick over budget for {over_ticks} ticks; {last}; {worst}; {budget}"
                        )
                    }
                }
            }
            Self::ProducerElected { prior_elder } => {
                format!("this renderer became producer after {prior_elder} aged out")
            }
            Self::ProducerDemoted { new_elder } => {
                format!("this renderer stopped producing; elder {new_elder}")
            }
            Self::RowConflict {
                agent_kind,
                agent_session_id,
                bound_pane,
                conflicting_pane,
            } => format!(
                "{agent_kind}/{agent_session_id} already on {bound_pane}; suppressed {conflicting_pane}"
            ),
            Self::DuplicatePaneId { pane_id } => format!("duplicate {pane_id} suppressed"),
            Self::ForeignSessionPane { pane_id, session } => {
                format!("dropped {pane_id} from session {session}")
            }
            Self::LocalSessionBindRejected {
                agent_kind,
                agent_session_id,
                pane_id,
                reason,
            } => format!(
                "rejected local {agent_kind}/{agent_session_id} bind to {pane_id}: {}",
                reason.as_str()
            ),
            Self::GhostSessionBind {
                agent_kind,
                agent_session_id,
                pane_id,
            } => {
                format!("local {agent_kind}/{agent_session_id} bound to re-launched pane {pane_id}")
            }
            Self::GroupMigration {
                pane_id, from, to, ..
            } => format!(
                "{pane_id} moved {}:{} -> {}:{}",
                from.kind, from.key, to.kind, to.key
            ),
            Self::NewbornQuarantined { pane_id } => {
                format!("held newborn {pane_id} until cwd resolves")
            }
            Self::MixedBuildWriters {
                prior_build,
                own_build,
            } => format!("prior frame from build {prior_build}; this producer is {own_build}"),
            Self::RendererPanic { message, .. } => message.clone(),
            Self::RendererSignalDeath {
                signal,
                exit_code,
                stderr_excerpt,
            } => {
                let reason = match (signal, exit_code) {
                    (Some(signal), _) => format!("signal {signal}"),
                    (None, Some(code)) => format!("exit {code}"),
                    (None, None) => "unknown termination".to_owned(),
                };
                let excerpt = stderr_excerpt.lines().last().unwrap_or(stderr_excerpt);
                format!("render worker died by {reason}: {excerpt}")
            }
            Self::RendererOrphanReaped {
                pane_id,
                worker_pid,
            } => format!("reaped orphaned renderer {worker_pid} after pane {pane_id} disappeared"),
            Self::SidebarOrphanReaped {
                pane_id,
                pid,
                first_confirmed_at_ms,
                second_confirmed_at_ms,
                sigkilled,
            } => format!(
                "reaped orphaned sidebar {pid} after pane {pane_id} was absent at {first_confirmed_at_ms} and {second_confirmed_at_ms}; sigkill={sigkilled}"
            ),
            Self::SubagentOrphanReaped {
                agent_kind,
                agent_id,
                parent_agent_id,
                orphaned_at_ms,
            } => format!(
                "reaped orphaned subagent {agent_kind}/{agent_id} after parent {parent_agent_id} remained ended or absent past grace (evidence timestamp {orphaned_at_ms})"
            ),
            Self::SubagentOrphanRepairFailed {
                agent_kind,
                agent_id,
                parent_agent_id,
                orphaned_at_ms,
                error,
            } => format!(
                "failed to repair orphaned subagent {agent_kind}/{agent_id} after parent {parent_agent_id} remained ended or absent past grace (evidence timestamp {orphaned_at_ms}): {error}"
            ),
            Self::PaneCacheDivergence {
                pane_id,
                pid,
                cache_observed_at_ms,
                authoritative_observed_at_ms,
            } => format!(
                "pane cache at {cache_observed_at_ms:?} omitted live sidebar {pid} in {pane_id}; authoritative roster observed it at {authoritative_observed_at_ms}"
            ),
            Self::SupervisorConvergence { target_build } => {
                format!("supervisor converging onto build {target_build}")
            }
            Self::SupervisorPreflightRejected {
                target_build,
                reason,
            } => format!("supervisor rejected build {target_build}: {reason}"),
            Self::SelfCloseRejected { siblings, reason } => {
                format!("self-close rejected ({siblings} siblings): {reason}")
            }
            Self::RendererExit { cause } => format!("renderer exited: {}", cause.as_str()),
            Self::FetchFoldStats {
                interval_ms,
                causes,
            } => format!(
                "fetch fold totals over {interval_ms}ms across {} causes",
                causes.len()
            ),
            Self::ToolLoopEscalated {
                agent_kind,
                agent_id,
                tool,
                count,
            } => format!("{agent_kind}/{agent_id} repeated {tool} {count} consecutive times"),
            Self::FrameAnomaly {
                anomaly:
                    AnomalyKind::RowPresenceFlap {
                        row_id,
                        gone_at_ms,
                        back_at_ms,
                        gap_evidence: Some(evidence),
                        ..
                    },
                ..
            } => {
                let pulled_pane = evidence
                    .pulled_pane_present
                    .map(|present| present.to_string())
                    .unwrap_or_else(|| "unknown".to_owned());
                format!(
                    "observed row_presence_flap on {row_id}; gap {}ms; pulled row present={}; pulled pane present={pulled_pane}",
                    back_at_ms.saturating_sub(*gone_at_ms),
                    evidence.pulled_row_present,
                )
            }
            Self::FrameAnomaly { anomaly, .. } => {
                let subject = anomaly
                    .subject()
                    .map(|subject| format!(" on {subject}"))
                    .unwrap_or_default();
                format!("observed {}{subject}", anomaly.key())
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

/// Topology proof attached to a pane-count drop while both frames are present.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDropEvidence {
    pub prior_panes: usize,
    pub fresh_panes: usize,
    #[serde(default)]
    pub mass_shrink: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_views: Vec<PaneDropViewEvidence>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneDropViewEvidence {
    pub view_id: String,
    pub prior_panes: usize,
    pub remaining_panes: usize,
    pub removed_pane_ids: Vec<PaneId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_panes: Vec<ManagedPaneEvidence>,
}

impl PaneDropViewEvidence {
    pub fn removed_completely(&self) -> bool {
        self.remaining_panes == 0 && self.removed_pane_ids.len() == self.prior_panes
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedPaneEvidence {
    pub pane_id: PaneId,
    pub agent_kind: AgentKind,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowPresenceGapEvidence {
    pub frame: FrameStamp,
    pub pulled_row_present: bool,
    pub pulled_pane_present: Option<bool>,
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
    pub(crate) fn identity(&self) -> String {
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
    pub(crate) fn is_spend_tally(&self) -> bool {
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap_evidence: Option<RowPresenceGapEvidence>,
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
    AgentCardWithoutProcess {
        row_id: String,
        pane_id: Option<String>,
        pid: u32,
        kind: String,
    },
}

impl AnomalyKind {
    pub(crate) fn key(&self) -> &'static str {
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
            Self::AgentCardWithoutProcess { .. } => "agent_card_without_process",
        }
    }

    /// The row/pane/group identity an anomaly is about. It composes
    /// [`DiagEvent::identity_key`], which the sink rate limit and Doctor's
    /// incident fold both key on; detectors with whole-frame scope have no
    /// subject.
    pub(crate) fn subject(&self) -> Option<Cow<'_, str>> {
        match self {
            Self::RowPresenceFlap { row_id, .. }
            | Self::ShortLivedRow { row_id, .. }
            | Self::ValueOscillation { row_id, .. }
            | Self::StatusChurn { row_id, .. }
            | Self::DuplicateRowId { row_id, .. }
            | Self::RowPaneMissingFromFrame { row_id, .. }
            | Self::DeadPid { row_id, .. }
            | Self::AgentCardWithoutProcess { row_id, .. } => Some(Cow::Borrowed(row_id)),
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
mod tests;
