use super::panel::{kv, two_column};
use super::*;
use std::collections::BTreeMap;

use rimz::harness::assist_log::{self, Assist, AssistRecord, AssistWindowReset};
use rimz::harness::auto_redeem::RedeemReason;
use rimz::ids::{AgentKind, AgentSessionId};
use rimz::store::event::SessionDeathCause;
use rimz::store::message::AutoCompact;

#[derive(Clone, Debug, Default, Serialize)]
pub(super) struct AssistStats {
    pub(super) window: String,
    pub(super) rollup: AssistRollup,
    pub(super) events: Vec<AssistEvent>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(super) struct AssistRollup {
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) workflow_starts: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) workflow_steps: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) workflow_routes: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) workflow_parks: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub(super) workflow_ends: usize,
    pub(super) molts: usize,
    pub(super) stall_notices: usize,
    pub(super) resident_launches: usize,
    pub(super) judge_declines: usize,
    pub(super) model_aliases: usize,
    pub(super) tier_fallbacks: usize,
    pub(super) redeems: usize,
    pub(super) resets: usize,
    pub(super) manual_redeems: usize,
    pub(super) manual_resets: usize,
    pub(super) resumes: usize,
    pub(super) recovered_secs: u64,
    pub(super) compacts: usize,
    pub(super) keepalives: usize,
    pub(super) idle_stops: usize,
    pub(super) hibernations: usize,
    pub(super) hibernate_reclaimed_bytes: u64,
    pub(super) hibernate_holds: BTreeMap<String, String>,
    pub(super) launch_retries: usize,
    pub(super) restores: usize,
    pub(super) restored_sessions: usize,
    pub(super) sweeps: usize,
    pub(super) reclaimed_bytes: u64,
    /// Attempts that did not take effect, keyed by the count they would have joined.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) held: BTreeMap<&'static str, usize>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case", tag = "assist")]
pub(super) enum AssistEvent {
    WorkflowStart {
        at: Timestamp,
        run: rimz::ids::WorkflowRunId,
        workflow: String,
        task: Option<String>,
        checkout: std::path::PathBuf,
        prompt: bool,
    },
    WorkflowStep {
        at: Timestamp,
        run: rimz::ids::WorkflowRunId,
        workflow: String,
        step: String,
        visit: u64,
        seq: u64,
        handle: Option<String>,
        run_id: Option<rimz::ids::RunId>,
    },
    WorkflowRoute {
        at: Timestamp,
        run: rimz::ids::WorkflowRunId,
        workflow: String,
        node: String,
        outcome: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<u64>,
    },
    WorkflowPark {
        at: Timestamp,
        run: rimz::ids::WorkflowRunId,
        workflow: String,
        node: String,
        reason: rimz::store::workflow::ParkReason,
        text: Option<String>,
    },
    WorkflowEnd {
        at: Timestamp,
        run: rimz::ids::WorkflowRunId,
        workflow: String,
        state: rimz::store::workflow::RunState,
    },
    Molt {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        label: String,
        successor_kind: AgentKind,
        successor_id: AgentSessionId,
        successor_label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        generation: Option<u8>,
    },
    JudgeDecline {
        at: Timestamp,
        task: String,
        checkout: std::path::PathBuf,
        profile: String,
        kind: AgentKind,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
    },
    StallNotice {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        parent: String,
        silent_secs: u64,
        message_id: String,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ResidentLaunch {
        at: Timestamp,
        task: String,
        checkout: std::path::PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        condition: Option<rimz::harness::schedule::when::ConditionEvidence>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        stopped: Vec<String>,
        handles: Vec<String>,
    },
    ModelAlias {
        at: Timestamp,
        kind: AgentKind,
        login: rimz::ids::LoginKey,
        alias: String,
        from: String,
        to: String,
    },
    TierFallback {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        profile: String,
        tier: rimz::config::tiers::ModelTier,
        model: String,
        skipped: Vec<rimz::agents::TierSkip>,
    },
    #[serde(rename = "auto_redeem")]
    Redeem {
        at: Timestamp,
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        login: Option<rimz::ids::LoginName>,
        reason: RedeemReason,
        request_id: String,
        credits: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        soonest_expiry: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        natural_reset: Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<String>,
        windows_reset: bool,
        window_resets: Vec<AssistWindowReset>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "auto_continue")]
    Continue {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        park: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        parked_since: Option<Timestamp>,
        delivered: bool,
        message_id: String,
    },
    #[serde(rename = "auto_compact")]
    Compact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        threshold: AutoCompact,
        #[serde(skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        message_id: String,
    },
    CacheKeepalive {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        waits: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        horizon_secs: Option<u64>,
        message_id: String,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        capped: bool,
    },
    #[serde(rename = "idle_compact")]
    IdleCompact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        idle_secs: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        idle_after_secs: Option<u64>,
        occupied_tokens: u64,
        message_id: String,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    IdleStop {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        label: String,
        idle_secs: u64,
        idle_after_secs: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        requested_by: Option<String>,
        stopped: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Hibernate {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        label: String,
        idle_secs: u64,
        frozen: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        reclaimed_bytes: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hold: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    LaunchRetry {
        at: Timestamp,
        kind: AgentKind,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        run_id: Option<rimz::RunId>,
        attempt: u8,
        #[serde(skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        startup_ms: u64,
        relaunched: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    FlipCompact {
        at: Timestamp,
        kind: AgentKind,
        agent_id: AgentSessionId,
        role: String,
        threshold: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        to: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        occupied_tokens: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        delivered: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    #[serde(rename = "auto_resume")]
    Resume {
        at: Timestamp,
        workspace_id: rimz::ids::WorkspaceId,
        session_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        cause: Option<SessionDeathCause>,
        recovered: usize,
        labels: Vec<String>,
    },
    #[serde(rename = "auto_gc")]
    Gc {
        at: Timestamp,
        workspace_id: rimz::ids::WorkspaceId,
        scope: rimz::harness::auto_gc::GcScope,
        older_than_secs: u64,
        reclaimed_bytes: u64,
        worktrees_removed: usize,
        workspaces_pruned: usize,
        files_removed: usize,
        messages_archived: usize,
        problems: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

impl AssistStats {
    pub(super) fn load(state_root: &Path, window: Window, now: Timestamp, timezone: &str) -> Self {
        let since = window.assist_since(now, timezone);
        Self::from_records(window.assist_label(), assist_log::recent(state_root, since))
    }

    pub(super) fn from_records(window: impl Into<String>, records: Vec<AssistRecord>) -> Self {
        let mut events = records
            .into_iter()
            .map(AssistEvent::from_record)
            .collect::<Vec<_>>();
        events.sort_by_key(AssistEvent::at);
        events.reverse();

        let mut rollup = AssistRollup::default();
        for event in &events {
            if let AssistEvent::Hibernate {
                kind,
                agent_id,
                hold: Some(hold),
                ..
            } = event
            {
                rollup
                    .hibernate_holds
                    .entry(format!("{kind}/{agent_id}"))
                    .or_insert_with(|| hold.clone());
            }
            if !event.took_effect() {
                *rollup.held.entry(event.count_key()).or_default() += 1;
                continue;
            }
            match event {
                AssistEvent::WorkflowStart { .. } => rollup.workflow_starts += 1,
                AssistEvent::WorkflowStep { .. } => rollup.workflow_steps += 1,
                AssistEvent::WorkflowRoute { .. } => rollup.workflow_routes += 1,
                AssistEvent::WorkflowPark { .. } => rollup.workflow_parks += 1,
                AssistEvent::WorkflowEnd { .. } => rollup.workflow_ends += 1,
                AssistEvent::Molt { .. } => rollup.molts += 1,
                AssistEvent::ResidentLaunch { .. } => rollup.resident_launches += 1,
                AssistEvent::JudgeDecline { .. } => rollup.judge_declines += 1,
                AssistEvent::ModelAlias { .. } => rollup.model_aliases += 1,
                AssistEvent::TierFallback { .. } => rollup.tier_fallbacks += 1,
                AssistEvent::Redeem {
                    outcome, reason, ..
                } => {
                    rollup.redeems += 1;
                    rollup.resets += usize::from(outcome.as_deref() == Some("reset"));
                    if *reason == RedeemReason::Manual {
                        rollup.manual_redeems += 1;
                        rollup.manual_resets += usize::from(outcome.as_deref() == Some("reset"));
                    }
                }
                AssistEvent::Continue {
                    at, parked_since, ..
                } => {
                    rollup.resumes += 1;
                    rollup.recovered_secs += recovered_secs(*parked_since, *at);
                }
                AssistEvent::StallNotice { .. } => rollup.stall_notices += 1,
                AssistEvent::Compact { .. } => rollup.compacts += 1,
                AssistEvent::CacheKeepalive { .. } => rollup.keepalives += 1,
                AssistEvent::IdleStop { .. } => rollup.idle_stops += 1,
                AssistEvent::Hibernate {
                    frozen,
                    reclaimed_bytes,
                    ..
                } => {
                    rollup.hibernations += usize::from(*frozen);
                    rollup.hibernate_reclaimed_bytes += reclaimed_bytes.unwrap_or(0);
                }
                AssistEvent::LaunchRetry { .. } => rollup.launch_retries += 1,
                AssistEvent::IdleCompact { .. } | AssistEvent::FlipCompact { .. } => {
                    rollup.compacts += 1
                }
                AssistEvent::Resume { recovered, .. } => {
                    rollup.restores += 1;
                    rollup.restored_sessions += recovered;
                }
                AssistEvent::Gc {
                    reclaimed_bytes, ..
                } => {
                    rollup.sweeps += 1;
                    rollup.reclaimed_bytes += reclaimed_bytes;
                }
            }
        }
        Self {
            window: window.into(),
            rollup,
            events,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub(super) fn timeline(&self, limit: usize, all: bool) -> (Vec<&AssistEvent>, usize) {
        let mut events: Vec<_> = self
            .events
            .iter()
            .filter(|event| all || event.took_effect())
            .collect();
        let shown = if limit == 0 {
            events.len()
        } else {
            limit.min(events.len())
        };
        let older = events.len() - shown;
        events.truncate(shown);
        (events, older)
    }
}

impl AssistEvent {
    fn took_effect(&self) -> bool {
        match self {
            // An undelivered notice is parked in the parent's queue; only an error means it failed.
            Self::StallNotice { error, .. } => error.is_none(),
            Self::Continue { delivered, .. }
            | Self::CacheKeepalive { delivered, .. }
            | Self::IdleCompact { delivered, .. }
            | Self::FlipCompact { delivered, .. } => *delivered,
            Self::IdleStop { stopped, .. } => *stopped,
            // A deepening step reclaims memory from an agent already frozen.
            Self::Hibernate {
                frozen,
                reclaimed_bytes,
                ..
            } => *frozen || reclaimed_bytes.is_some(),
            Self::LaunchRetry { relaunched, .. } => *relaunched,
            // A stamp write fails after the reset, so an error does not undo it.
            Self::Redeem { windows_reset, .. } => *windows_reset,
            Self::Gc { error, .. } => error.is_none(),
            Self::Molt { .. }
            | Self::WorkflowStart { .. }
            | Self::WorkflowStep { .. }
            | Self::WorkflowRoute { .. }
            | Self::WorkflowPark { .. }
            | Self::WorkflowEnd { .. }
            | Self::JudgeDecline { .. }
            | Self::ResidentLaunch { .. }
            | Self::ModelAlias { .. }
            | Self::TierFallback { .. }
            | Self::Compact { .. }
            | Self::Resume { .. } => true,
        }
    }

    /// The `AssistRollup` field this event counts toward, named as it serializes.
    fn count_key(&self) -> &'static str {
        match self {
            Self::WorkflowStart { .. } => "workflow_starts",
            Self::WorkflowStep { .. } => "workflow_steps",
            Self::WorkflowRoute { .. } => "workflow_routes",
            Self::WorkflowPark { .. } => "workflow_parks",
            Self::WorkflowEnd { .. } => "workflow_ends",
            Self::Molt { .. } => "molts",
            Self::JudgeDecline { .. } => "judge_declines",
            Self::StallNotice { .. } => "stall_notices",
            Self::ResidentLaunch { .. } => "resident_launches",
            Self::ModelAlias { .. } => "model_aliases",
            Self::TierFallback { .. } => "tier_fallbacks",
            Self::Redeem {
                reason: RedeemReason::Manual,
                ..
            } => "manual_redeems",
            Self::Redeem { .. } => "redeems",
            Self::Continue { .. } => "resumes",
            Self::Compact { .. } | Self::IdleCompact { .. } | Self::FlipCompact { .. } => {
                "compacts"
            }
            Self::CacheKeepalive { .. } => "keepalives",
            Self::IdleStop { .. } => "idle_stops",
            Self::Hibernate { .. } => "hibernations",
            Self::LaunchRetry { .. } => "launch_retries",
            Self::Resume { .. } => "restores",
            Self::Gc { .. } => "sweeps",
        }
    }

    fn from_record(record: AssistRecord) -> Self {
        match record.assist {
            Assist::WorkflowStart {
                run,
                workflow,
                task,
                checkout,
                prompt,
            } => Self::WorkflowStart {
                at: record.at,
                run,
                workflow,
                task,
                checkout,
                prompt,
            },
            Assist::WorkflowStep {
                run,
                workflow,
                step,
                visit,
                seq,
                handle,
                run_id,
            } => Self::WorkflowStep {
                at: record.at,
                run,
                workflow,
                step,
                visit,
                seq,
                handle,
                run_id,
            },
            Assist::WorkflowRoute {
                run,
                workflow,
                node,
                outcome,
                to,
                count,
            } => Self::WorkflowRoute {
                at: record.at,
                run,
                workflow,
                node,
                outcome,
                to,
                count,
            },
            Assist::WorkflowPark {
                run,
                workflow,
                node,
                reason,
                text,
            } => Self::WorkflowPark {
                at: record.at,
                run,
                workflow,
                node,
                reason,
                text,
            },
            Assist::WorkflowEnd {
                run,
                workflow,
                state,
            } => Self::WorkflowEnd {
                at: record.at,
                run,
                workflow,
                state,
            },
            Assist::Molt {
                kind,
                agent_id,
                label,
                successor_kind,
                successor_id,
                successor_label,
                generation,
            } => Self::Molt {
                at: record.at,
                kind,
                agent_id,
                label,
                successor_kind,
                successor_id,
                successor_label,
                generation,
            },
            Assist::JudgeDecline {
                task,
                checkout,
                profile,
                kind,
                reason,
                cost_usd,
            } => Self::JudgeDecline {
                at: record.at,
                task,
                checkout,
                profile,
                kind,
                reason,
                cost_usd,
            },
            Assist::StallNotice {
                kind,
                agent_id,
                label,
                parent,
                silent_secs,
                message_id,
                delivered,
                error,
            } => Self::StallNotice {
                at: record.at,
                kind,
                agent_id,
                label,
                parent,
                silent_secs,
                message_id,
                delivered,
                error,
            },
            Assist::ResidentLaunch {
                task,
                checkout,
                condition,
                stopped,
                handles,
            } => Self::ResidentLaunch {
                at: record.at,
                task,
                checkout,
                condition,
                stopped,
                handles,
            },
            Assist::ModelAlias {
                kind,
                login,
                alias,
                from,
                to,
            } => Self::ModelAlias {
                at: record.at,
                kind,
                login,
                alias,
                from,
                to,
            },
            Assist::TierFallback {
                kind,
                agent_id,
                label,
                profile,
                tier,
                model,
                skipped,
            } => Self::TierFallback {
                at: record.at,
                kind,
                agent_id,
                label,
                profile,
                tier,
                model,
                skipped,
            },
            Assist::AutoRedeem {
                kind,
                login,
                reason,
                request_id,
                credits,
                soonest_expiry,
                natural_reset,
                outcome,
                windows_reset,
                window_resets,
                error,
            } => Self::Redeem {
                at: record.at,
                kind,
                login,
                reason,
                request_id,
                credits,
                soonest_expiry,
                natural_reset,
                outcome,
                windows_reset,
                window_resets,
                error,
            },
            Assist::AutoContinue {
                kind,
                agent_id,
                label,
                park,
                parked_since,
                delivered,
                message_id,
            } => Self::Continue {
                at: record.at,
                kind,
                agent_id,
                label,
                park,
                parked_since,
                delivered,
                message_id,
            },
            Assist::AutoCompact {
                kind,
                agent_id,
                label,
                threshold,
                occupied_tokens,
                message_id,
            } => Self::Compact {
                at: record.at,
                kind,
                agent_id,
                label,
                threshold,
                occupied_tokens,
                message_id,
            },
            Assist::CacheKeepalive {
                kind,
                agent_id,
                label,
                idle_secs,
                waits,
                horizon_secs,
                message_id,
                delivered,
                error,
                capped,
            } => Self::CacheKeepalive {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                waits,
                horizon_secs,
                message_id,
                delivered,
                error,
                capped,
            },
            Assist::IdleStop {
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                requested_by,
                stopped,
                error,
            } => Self::IdleStop {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                requested_by,
                stopped,
                error,
            },
            Assist::Hibernate {
                kind,
                agent_id,
                label,
                idle_secs,
                frozen,
                reclaimed_bytes,
                hold,
                error,
            } => Self::Hibernate {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                frozen,
                reclaimed_bytes,
                hold,
                error,
            },
            Assist::LaunchRetry {
                kind,
                label,
                run_id,
                attempt,
                exit_code,
                startup_ms,
                relaunched,
                error,
            } => Self::LaunchRetry {
                at: record.at,
                kind,
                label,
                run_id,
                attempt,
                exit_code,
                startup_ms,
                relaunched,
                error,
            },
            Assist::IdleCompact {
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                occupied_tokens,
                message_id,
                delivered,
                error,
            } => Self::IdleCompact {
                at: record.at,
                kind,
                agent_id,
                label,
                idle_secs,
                idle_after_secs,
                occupied_tokens,
                message_id,
                delivered,
                error,
            },
            Assist::FlipCompact {
                kind,
                agent_id,
                role,
                threshold,
                from,
                to,
                occupied_tokens,
                message_id,
                delivered,
                error,
            } => Self::FlipCompact {
                at: record.at,
                kind,
                agent_id,
                role,
                threshold,
                from,
                to,
                occupied_tokens,
                message_id,
                delivered,
                error,
            },
            Assist::AutoResume {
                workspace_id,
                session_name,
                cause,
                recovered,
                labels,
            } => Self::Resume {
                at: record.at,
                workspace_id,
                session_name,
                cause,
                recovered,
                labels,
            },
            Assist::AutoGc {
                workspace_id,
                scope,
                older_than_secs,
                reclaimed_bytes,
                worktrees_removed,
                workspaces_pruned,
                files_removed,
                messages_archived,
                problems,
                error,
                ..
            } => Self::Gc {
                at: record.at,
                workspace_id,
                scope,
                older_than_secs,
                reclaimed_bytes,
                worktrees_removed,
                workspaces_pruned,
                files_removed,
                messages_archived,
                problems,
                error,
            },
        }
    }

    fn at(&self) -> Timestamp {
        match self {
            Self::Molt { at, .. }
            | Self::WorkflowStart { at, .. }
            | Self::WorkflowStep { at, .. }
            | Self::WorkflowRoute { at, .. }
            | Self::WorkflowPark { at, .. }
            | Self::WorkflowEnd { at, .. }
            | Self::StallNotice { at, .. }
            | Self::ResidentLaunch { at, .. }
            | Self::JudgeDecline { at, .. }
            | Self::ModelAlias { at, .. }
            | Self::Redeem { at, .. }
            | Self::Continue { at, .. }
            | Self::Compact { at, .. }
            | Self::IdleCompact { at, .. }
            | Self::IdleStop { at, .. }
            | Self::Hibernate { at, .. }
            | Self::LaunchRetry { at, .. }
            | Self::CacheKeepalive { at, .. }
            | Self::FlipCompact { at, .. }
            | Self::Resume { at, .. }
            | Self::Gc { at, .. }
            | Self::TierFallback { at, .. } => *at,
        }
    }
}

impl Window {
    pub(super) fn assist_since(self, now: Timestamp, timezone: &str) -> Option<Timestamp> {
        let days = match self {
            Self::Today => {
                let now_secs = u64::try_from(now.as_second()).ok()?;
                let cutoff =
                    rimz::agents::spending::local_day_start_secs(now_secs, Some(timezone))?;
                return Timestamp::from_second(i64::try_from(cutoff).ok()?).ok();
            }
            Self::Week => 7,
            Self::Month => 30,
            Self::Year => 365,
        };
        Some(now - Duration::from_secs(days * DAY_SECS as u64))
    }

    fn assist_label(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "7d",
            Self::Month => "30d",
            Self::Year => "1y",
        }
    }
}

pub(super) fn panel_lines(lines: &mut Vec<String>, stats: &AssistStats, panel_width: usize) {
    let rows = category_rows(&stats.rollup);
    if rows.is_empty() {
        return;
    }
    lines.push(format!(
        "  {}",
        render::paint(render::palette::header(), "Assists")
    ));
    let split = rows.len().div_ceil(2);
    two_column(lines, &rows[..split], &rows[split..], panel_width);
}

pub(super) fn render_full(stats: &AssistStats, limit: usize, all: bool, json: bool) -> Result<()> {
    if json {
        let as_of = Timestamp::now().to_zoned(crate::cli::machine_config().time_zone());
        return render::json_pretty(&super::json::assists_value(stats, limit, all, &as_of));
    }
    let (events, older) = stats.timeline(limit, all);
    let mut out = render::out();
    if stats.is_empty() {
        writeln!(out, "no assists recorded")?;
        return Ok(());
    }
    let categories = category_entries(&stats.rollup)
        .into_iter()
        .map(|(label, value)| format!("{label} {value}"))
        .collect::<Vec<_>>()
        .join(" · ");
    write!(out, "assists ({})", stats.window)?;
    if !categories.is_empty() {
        write!(out, " — {categories}")?;
    }
    writeln!(out)?;
    let zone = MachineConfig::load_lenient().time_zone();
    for event in events {
        writeln!(out, "{}", forensic_line(event, &zone))?;
    }
    if older > 0 {
        writeln!(out, "… {older} older; rimz stats --assists -n 0 for all")?;
    }
    Ok(())
}

pub(super) fn category_rows(rollup: &AssistRollup) -> Vec<String> {
    category_entries(rollup)
        .into_iter()
        .map(|(label, value)| kv(label, &value))
        .collect()
}

fn category_entries(rollup: &AssistRollup) -> Vec<(&'static str, String)> {
    let mut rows = Vec::with_capacity(5);
    let mut row = |label: &'static str, key: &str, count: usize, detail: Option<String>| {
        let held = rollup.held.get(key).copied().unwrap_or(0);
        if count == 0 && held == 0 {
            return;
        }
        let notes = detail
            .into_iter()
            .chain((held > 0).then(|| format!("{held} held")))
            .collect::<Vec<_>>();
        let value = if notes.is_empty() {
            count.to_string()
        } else {
            format!("{count} ({})", notes.join(", "))
        };
        rows.push((label, value));
    };
    row("Stall notice:", "stall_notices", rollup.stall_notices, None);
    let launches = rollup.resident_launches;
    row(
        "Workflow:",
        "workflow",
        rollup.workflow_starts
            + rollup.workflow_steps
            + rollup.workflow_routes
            + rollup.workflow_parks
            + rollup.workflow_ends,
        None,
    );
    row("Resident launches:", "resident_launches", launches, None);
    let declines = rollup.judge_declines;
    row("Judge declines:", "judge_declines", declines, None);
    row(
        "Model aliases:",
        "model_aliases",
        rollup.model_aliases,
        None,
    );
    row(
        "Tier fallback:",
        "tier_fallbacks",
        rollup.tier_fallbacks,
        None,
    );
    let recovered =
        (rollup.recovered_secs > 0).then(|| format!("+{}", format_hours(rollup.recovered_secs)));
    row("Auto-continue:", "resumes", rollup.resumes, recovered);
    row("Auto-compact:", "compacts", rollup.compacts, None);
    row("Keepalive:", "keepalives", rollup.keepalives, None);
    row("Idle stop:", "idle_stops", rollup.idle_stops, None);
    row("Molts:", "molts", rollup.molts, None);
    let reclaimed = (rollup.hibernations > 0).then(|| {
        let bytes = render::fmt_bytes(rollup.hibernate_reclaimed_bytes);
        format!("{bytes} reclaimed")
    });
    row("Hibernate:", "hibernations", rollup.hibernations, reclaimed);
    row(
        "Launch retry:",
        "launch_retries",
        rollup.launch_retries,
        None,
    );
    let auto_redeems = rollup.redeems - rollup.manual_redeems;
    let auto_resets = rollup.resets - rollup.manual_resets;
    let resets = (auto_resets > 0).then(|| format!("{auto_resets} reset{}", plural(auto_resets)));
    row("Auto-redeem:", "redeems", auto_redeems, resets);
    let agents = (rollup.restores > 0).then(|| {
        let restored = rollup.restored_sessions;
        format!("{restored} agent{}", plural(restored))
    });
    row("Auto-resume:", "restores", rollup.restores, agents);
    let freed = (rollup.reclaimed_bytes > 0).then(|| render::fmt_bytes(rollup.reclaimed_bytes));
    row("Auto-gc:", "sweeps", rollup.sweeps, freed);
    rows
}

pub(super) fn benefit_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone());
    let time = at.strftime("%H:%M");
    match event {
        AssistEvent::WorkflowStart { workflow, task, .. } => format!(
            "{time} workflow {workflow} started{}",
            task.as_ref()
                .map_or_else(String::new, |task| format!(" by loop {task}"))
        ),
        AssistEvent::WorkflowStep {
            workflow,
            step,
            visit,
            ..
        } => format!("{time} workflow {workflow} {step} launched (visit {visit})"),
        AssistEvent::WorkflowRoute {
            workflow,
            node,
            outcome,
            to,
            count,
            ..
        } => match count {
            Some(count) => format!("{time} workflow {workflow} {node} waited {outcome} ×{count}"),
            None => format!("{time} workflow {workflow} {node} said {outcome} → {to}"),
        },
        AssistEvent::WorkflowPark {
            workflow,
            node,
            reason,
            text,
            ..
        } => format!(
            "{time} workflow {workflow} {node} parked: {}{}",
            reason.as_str(),
            text.as_ref()
                .map_or_else(String::new, |text| format!(": {}", first_line(text)))
        ),
        AssistEvent::WorkflowEnd {
            workflow, state, ..
        } => format!("{time} workflow {workflow} {}", state.as_str()),
        AssistEvent::Molt {
            label,
            successor_label,
            ..
        } => {
            format!("{time} ⤳ {label} molted into {successor_label}")
        }
        AssistEvent::StallNotice {
            kind,
            label,
            parent,
            silent_secs,
            error,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let silence =
                rimz::utils::time::format_duration_compact(Duration::from_secs(*silent_secs));
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            format!("{time} {agent} silent {silence} — stall notice to {parent}{error}")
        }
        AssistEvent::ResidentLaunch { task, handles, .. } => {
            format!("{time} ▶ loop {task} opened {}", handles.join(", "))
        }
        AssistEvent::JudgeDecline {
            task,
            profile,
            reason,
            ..
        } => {
            format!(
                "{time} loop {task} declined by {profile}: {}",
                first_line(reason)
            )
        }
        AssistEvent::ModelAlias {
            kind,
            alias,
            from,
            to,
            ..
        } => format!("{time} {kind} alias {alias} now resolves to {to} (was {from})"),
        AssistEvent::TierFallback {
            profile,
            model,
            skipped,
            ..
        } => {
            let first = skipped
                .first()
                .map(|skip| format!("{} → {model} ({})", skip.model, skip.reason))
                .unwrap_or_else(|| model.clone());
            format!("{time} ↘ {profile}: {first}")
        }
        AssistEvent::Redeem {
            kind,
            login,
            reason,
            outcome,
            error,
            ..
        } => {
            let named_login = login
                .as_ref()
                .filter(|login| !login.is_default())
                .map(|login| format!("@{login}"))
                .unwrap_or_default();
            let result = match (outcome.as_deref(), error.as_deref()) {
                (Some("reset"), _) => "budget reset ✓".to_owned(),
                (Some(outcome), _) => outcome.replace('_', " "),
                (None, Some(error)) => format!("failed: {}", first_line(error)),
                (None, None) => "request failed".to_owned(),
            };
            format!(
                "{time} ↻ {kind}{named_login} credit — {} → {result}",
                reason_label(*reason)
            )
        }
        AssistEvent::Continue {
            at,
            kind,
            label,
            park,
            parked_since,
            delivered,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let action = if *delivered { "resumed" } else { "resume held" };
            let span = parked_since.map(|parked| {
                format!(
                    ", {}→{} ({} recovered)",
                    parked.to_zoned(zone.clone()).strftime("%H:%M"),
                    at.to_zoned(zone.clone()).strftime("%H:%M"),
                    format_hours(recovered_secs(Some(parked), *at))
                )
            });
            format!(
                "{time} ▶ {agent} {action} — {}{}",
                park_label(park),
                span.unwrap_or_default()
            )
        }
        AssistEvent::Compact {
            kind,
            label,
            occupied_tokens,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let detail = occupied_tokens
                .map(|tokens| {
                    format!(
                        " — {} ctx cleared before delivery",
                        compact_token_count(tokens)
                    )
                })
                .unwrap_or_default();
            format!("{time} ⌁ {agent} auto-compact{detail}")
        }
        AssistEvent::CacheKeepalive {
            kind,
            label,
            idle_secs,
            waits,
            horizon_secs,
            error,
            capped,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let compact =
                |secs| rimz::utils::time::format_duration_compact(Duration::from_secs(secs));
            let idle = compact(*idle_secs);
            let holding = horizon_secs
                .map(|horizon| format!(", holding warm ({})", compact(horizon)))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let noun = if *waits == 1 { "wait" } else { "waits" };
            let waits = if *waits == 0 && horizon_secs.is_some() {
                String::new()
            } else {
                format!(" — {waits} {noun} pending")
            };
            let limit = if *capped { ", limit reached" } else { "" };
            format!("{time} ◷ {agent} cache keepalive after {idle}{holding}{waits}{error}{limit}")
        }
        AssistEvent::IdleStop {
            label,
            idle_secs,
            idle_after_secs,
            requested_by,
            stopped,
            error,
            ..
        } => {
            let outcome = if *stopped { "stopped" } else { "stop failed" };
            let compact =
                |secs: u64| rimz::utils::time::format_duration_compact(Duration::from_secs(secs));
            let requester = requested_by
                .as_deref()
                .map(|by| format!(" — requested by {by}"))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            format!(
                "{time} ■ {label} idle {outcome} after {} (threshold {}){requester}{error}",
                compact(*idle_secs),
                compact(*idle_after_secs),
            )
        }
        AssistEvent::Hibernate {
            label,
            frozen,
            reclaimed_bytes,
            hold,
            error,
            ..
        } => {
            if *frozen {
                format!(
                    "{time} {label} hibernated, {} reclaimed",
                    render::fmt_bytes(reclaimed_bytes.unwrap_or(0))
                )
            } else if let Some(bytes) = reclaimed_bytes {
                format!(
                    "{time} {label} hibernation deepened, {} reclaimed",
                    render::fmt_bytes(*bytes)
                )
            } else {
                format!(
                    "{time} {label} hibernation held: {}",
                    hold.as_deref().or(error.as_deref()).unwrap_or("unknown")
                )
            }
        }
        AssistEvent::LaunchRetry {
            kind,
            label,
            attempt,
            exit_code,
            startup_ms,
            relaunched,
            error,
            ..
        } => {
            let exit = exit_code.map_or_else(
                || "on a signal".to_owned(),
                |code| format!("with code {code}"),
            );
            let startup = if *startup_ms < 1000 {
                "<1s".to_owned()
            } else {
                rimz::utils::time::format_duration_compact(Duration::from_secs(startup_ms / 1000))
            };
            let outcome = if *relaunched {
                format!("relaunched, attempt {attempt}")
            } else {
                format!("relaunch failed, attempt {attempt}")
            };
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            format!(
                "{time} ↺ {label} {kind} exited {exit} after {startup} before its session opened — {outcome}{error}"
            )
        }
        AssistEvent::IdleCompact {
            kind,
            label,
            idle_secs,
            idle_after_secs,
            occupied_tokens,
            delivered,
            error,
            ..
        } => {
            let agent = label.as_deref().unwrap_or(kind.as_str());
            let outcome = if *delivered {
                "compacted"
            } else {
                "compact held"
            };
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let (idle, threshold) = idle_after_secs.map_or_else(
                || (format_hours(*idle_secs), String::new()),
                |threshold| {
                    (
                        rimz::utils::time::format_duration_compact(std::time::Duration::from_secs(
                            *idle_secs,
                        )),
                        format!(
                            " (threshold {})",
                            rimz::utils::time::format_duration_compact(
                                std::time::Duration::from_secs(threshold)
                            )
                        ),
                    )
                },
            );
            format!(
                "{time} ⌁ {agent} idle {outcome} after {idle}{threshold} — {} ctx{error}",
                compact_token_count(*occupied_tokens),
            )
        }
        AssistEvent::FlipCompact {
            role,
            threshold,
            from,
            to,
            occupied_tokens,
            delivered,
            error,
            ..
        } => {
            let agent = format!("@{role}");
            let outcome = if *delivered { "" } else { " held" };
            let from = from.as_deref().unwrap_or("(none)");
            let context = occupied_tokens
                .map(|tokens| format!(" — {} ctx", compact_token_count(tokens)))
                .unwrap_or_default();
            let error = error
                .as_deref()
                .map(|error| format!(" ({})", first_line(error)))
                .unwrap_or_default();
            let threshold = compact_token_count(*threshold);
            format!(
                "{time} ⌁ {agent} flip compaction{outcome} — {from} → {to}{context}, threshold {threshold}{error}"
            )
        }
        AssistEvent::Resume {
            cause,
            recovered,
            labels,
            ..
        } => {
            let cause = cause.map_or_else(String::new, |cause| format!(" after {cause}"));
            let labels = if labels.is_empty() {
                String::new()
            } else {
                format!(" ({})", labels.join(", "))
            };
            format!(
                "{time} ⟲ rebirth recovery — {recovered} agent{} restored{cause}{labels}",
                plural(*recovered)
            )
        }
        AssistEvent::Gc {
            scope,
            reclaimed_bytes,
            worktrees_removed,
            workspaces_pruned,
            files_removed,
            messages_archived,
            problems,
            error: None,
            ..
        } => {
            let facts = [
                (*worktrees_removed, "worktree"),
                (*workspaces_pruned, "workspace"),
                (*files_removed, "file"),
                (*messages_archived, "message"),
                (*problems, "problem"),
            ]
            .into_iter()
            .filter(|(count, _)| *count > 0)
            .map(|(count, noun)| format!(", {count} {noun}{}", plural(count)))
            .collect::<String>();
            let scope = match scope {
                rimz::harness::auto_gc::GcScope::Room => "",
                rimz::harness::auto_gc::GcScope::Machine => "machine ",
            };
            format!(
                "{time} ♻ {scope}gc swept — {} reclaimed{facts}",
                render::fmt_bytes(*reclaimed_bytes)
            )
        }
        AssistEvent::Gc {
            error: Some(error), ..
        } => format!("{time} ♻ gc failed — {}", first_line(error)),
    }
}

pub(super) fn forensic_line(event: &AssistEvent, zone: &jiff::tz::TimeZone) -> String {
    let at = event.at().to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M");
    let benefit = benefit_line(event, zone)
        .split_once(' ')
        .map_or_else(|| benefit_line(event, zone), |(_, rest)| rest.to_owned());
    match event {
        AssistEvent::WorkflowStart { run, .. }
        | AssistEvent::WorkflowStep { run, .. }
        | AssistEvent::WorkflowRoute { run, .. }
        | AssistEvent::WorkflowPark { run, .. }
        | AssistEvent::WorkflowEnd { run, .. } => format!("{at} {benefit} · run {run}"),
        AssistEvent::Molt {
            kind,
            agent_id,
            successor_kind,
            successor_id,
            generation,
            ..
        } => format!(
            "{at} {benefit} · {kind} agent {agent_id} → {successor_kind} launch {successor_id}{}",
            generation.map_or_else(String::new, |generation| format!(
                " · generation {generation}"
            )),
        ),
        AssistEvent::ResidentLaunch {
            checkout,
            condition,
            stopped,
            ..
        } => format!(
            "{at} {benefit} · checkout {}{}{}",
            checkout.display(),
            condition
                .as_ref()
                .map_or_else(String::new, |evidence| format!(" · when {}", evidence.when)),
            if stopped.is_empty() {
                String::new()
            } else {
                format!(" · took over from {}", stopped.join(", "))
            },
        ),
        AssistEvent::JudgeDecline {
            checkout, cost_usd, ..
        } => format!(
            "{at} {benefit} · checkout {}{}",
            checkout.display(),
            cost_usd.map_or_else(String::new, |cost| format!(
                " · {}",
                rimz::theme::fmt::dollars2(cost)
            )),
        ),
        AssistEvent::ModelAlias { login, .. } => format!("{at} {benefit} · login {login}"),
        AssistEvent::TierFallback { agent_id, .. } => format!("{at} {benefit} · agent {agent_id}"),
        AssistEvent::Redeem {
            request_id,
            credits,
            soonest_expiry,
            natural_reset,
            window_resets,
            ..
        } => format!(
            "{at} {benefit} · request {request_id} · {credits} credit{}{}{}{}",
            plural(*credits as usize),
            timestamp_fact("expiry", *soonest_expiry, zone),
            timestamp_fact("natural reset", *natural_reset, zone),
            reset_facts(window_resets, zone),
        ),
        AssistEvent::StallNotice {
            agent_id,
            message_id,
            delivered,
            ..
        }
        | AssistEvent::Continue {
            agent_id,
            message_id,
            delivered,
            ..
        }
        | AssistEvent::CacheKeepalive {
            agent_id,
            message_id,
            delivered,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · delivered {delivered}"
        ),
        AssistEvent::Compact {
            agent_id,
            message_id,
            threshold,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · threshold {}",
            compact_threshold(*threshold),
        ),
        AssistEvent::IdleStop {
            agent_id, stopped, ..
        } => format!("{at} {benefit} · agent {agent_id} · stopped {stopped}"),
        AssistEvent::Hibernate {
            agent_id, frozen, ..
        } => format!("{at} {benefit} · agent {agent_id} · frozen {frozen}"),
        AssistEvent::LaunchRetry {
            run_id, relaunched, ..
        } => {
            let run = run_id
                .as_ref()
                .map(|run_id| format!(" · run {run_id}"))
                .unwrap_or_default();
            format!("{at} {benefit}{run} · relaunched {relaunched}")
        }
        AssistEvent::IdleCompact {
            agent_id,
            message_id,
            delivered,
            ..
        } => format!(
            "{at} {benefit} · agent {agent_id} · message {message_id} · delivered {delivered}"
        ),
        AssistEvent::FlipCompact {
            agent_id,
            message_id,
            delivered,
            ..
        } => {
            let message = message_id
                .as_deref()
                .map(|id| format!(" · message {id}"))
                .unwrap_or_default();
            format!("{at} {benefit} · agent {agent_id}{message} · delivered {delivered}")
        }
        AssistEvent::Resume {
            workspace_id,
            session_name,
            ..
        } => format!("{at} {benefit} · workspace {workspace_id} · session {session_name}"),
        AssistEvent::Gc {
            workspace_id,
            older_than_secs,
            ..
        } => format!(
            "{at} {benefit} · workspace {workspace_id} · cutoff {}",
            rimz::utils::time::format_duration_compact(Duration::from_secs(*older_than_secs))
        ),
    }
}

fn compact_token_count(tokens: u64) -> String {
    rimz::theme::fmt::compact_count(tokens)
}

fn is_zero(count: &usize) -> bool {
    *count == 0
}

fn compact_threshold(threshold: AutoCompact) -> String {
    match threshold {
        AutoCompact::Percent(percent) => format!("{percent}%"),
        AutoCompact::Tokens(tokens) => compact_token_count(tokens),
    }
}

fn reset_facts(windows: &[AssistWindowReset], zone: &jiff::tz::TimeZone) -> String {
    let facts = windows
        .iter()
        .map(|window| {
            let duration = window
                .duration_mins
                .map(rimz::theme::fmt::duration_label)
                .unwrap_or_else(|| "window".to_owned());
            let reset = window
                .resets_at
                .map(|reset| {
                    reset
                        .to_zoned(zone.clone())
                        .strftime("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_else(|| "unknown".to_owned());
            format!("{duration}→{reset}")
        })
        .collect::<Vec<_>>();
    if facts.is_empty() {
        String::new()
    } else {
        format!(" · windows {}", facts.join(", "))
    }
}

fn timestamp_fact(label: &str, timestamp: Option<Timestamp>, zone: &jiff::tz::TimeZone) -> String {
    timestamp
        .map(|timestamp| {
            format!(
                " · {label} {}",
                timestamp.to_zoned(zone.clone()).strftime("%Y-%m-%d %H:%M")
            )
        })
        .unwrap_or_default()
}

fn recovered_secs(parked_since: Option<Timestamp>, at: Timestamp) -> u64 {
    parked_since
        .map(|parked| at.duration_since(parked).as_secs().max(0) as u64)
        .unwrap_or(0)
}

fn format_hours(seconds: u64) -> String {
    format!("{:.1}h", seconds as f64 / 3_600.0)
}

fn reason_label(reason: RedeemReason) -> &'static str {
    match reason {
        RedeemReason::Manual => "manual",
        RedeemReason::ExpiryRescue => "expiry rescue",
        RedeemReason::BlockedGain => "blocked gain",
        RedeemReason::DoomedCredit => "doomed credit",
        RedeemReason::ScheduledRedeem => "scheduled redeem",
    }
}

fn park_label(park: &str) -> &str {
    if park.contains("rate_limit") {
        "limit park"
    } else if park.contains("overload") {
        "overload park"
    } else if park.contains("budget") {
        "budget park"
    } else {
        "API error park"
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

#[cfg(test)]
mod tests;
