//! Producer-side auto-continue: resume a parked agent through the message queue
//! after a rate-limit window resets or a retry backoff elapses.
//!
//! Opt-in ([`ResumeConfig::auto_continue`]). The producer arms the resume while
//! the park is fresh, re-arms a lost limit record after budget recovery when
//! the agent still carries a limit marker, and fires it once the class-specific
//! clock is due. The durable record carries everything needed between arm and
//! fire so the decision never depends on the ephemeral per-session context
//! surviving the wait:
//!
//! - **Arm.** Each frame an agent is parked on a resumable certificate
//!   ([`resume_park`]), the producer writes a durable [`ParkRecord`]
//!   capturing the park class and the agent's frozen `last_activity`. A
//!   rate-limit record captures the latest spent-window reset deadline; a
//!   backoff record carries the turn-error marker time and retry state.
//! - **Fire.** Once the window reset deadline or retry backoff is due and the
//!   agent is still idle (`last_activity` has not advanced), the producer
//!   spawns the detached `rimz agents auto-continue` helper that queues and
//!   delivers a resume-gated message record.
//! - **Clear.** Any activity since the park (the nudge took, or the agent woke on
//!   its own) advances `last_activity`, and the stale record is removed. A
//!   delivered resume message also clears the record; evidenced resume messages
//!   control exhaustion, while helper spawns only pace retries.
//!
//! This module owns only the durable record, the pane join, and the spawn — the
//! arm decision is the pure, unit-tested [`resume_park`].

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::SidebarSnapshot;
use crate::agents::{
    AgentCardRef, AgentState, ProviderCapacity, TurnErrorClass, display_turn_error,
    effective_turn_error_class,
};
use crate::config::{DEFAULT_AUTO_CONTINUE_BACKOFF_SECS, ResumeConfig};
use crate::ids::{AgentKind, AgentSessionId, MessageId, PaneId, WorkspaceId};
use crate::message::{DeliveryGate, MessageBody, MessageRecord, MessageStatus};
use crate::store::atomic::write_temp_then_rename_cache;
#[cfg(test)]
use crate::store::snapshot::PaneAgent;
use crate::store::snapshot::ResumeOutcome;

/// Minimum gap between auto-continue nudges to one rate-limit-parked agent. One
/// nudge resumes the turn within a frame, so this mostly bounds the brief window
/// before the agent's first hook lands; if a nudge fails to wake a still-parked
/// agent, RimZ retries on this cadence rather than typing every frame.
const AUTO_CONTINUE_RETRY_INTERVAL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoContinueRequest {
    pub workspace_id: WorkspaceId,
    pub kind: AgentKind,
    pub agent_id: AgentSessionId,
    pub pane_id: PaneId,
    pub message_id: Option<MessageId>,
    pub parked_since: Timestamp,
    pub text: String,
    pub reason: String,
    pub label: Option<String>,
}

/// A durable record of one park: written while the park is fresh, read after its
/// class-specific resume condition is due. It outlives the per-session context
/// the park was first seen through, so a resume survives both an expired context
/// sidecar and a fresh non-spent reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ParkRecord {
    /// The park class and its durable resume facts.
    kind: ParkKind,
    /// The agent's rollup `last_activity` at arm time. Equal or regressed means
    /// the agent has done nothing since: still parked, safe to nudge. Advanced
    /// means it woke (our nudge took, or it resumed on its own), so the record
    /// is stale.
    parked_at_activity: Timestamp,
    /// When the last auto-continue attempt fired, throttling re-nudges so a nudge
    /// that fails to wake a still-parked agent is retried without spamming a
    /// working one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_nudge_at: Option<Timestamp>,
    /// Auto-continue helper spawns for this park. Rate-limit and backoff records
    /// both use it for pacing and overload backoff steps; evidenced resume
    /// messages own the retry cap.
    #[serde(default)]
    retries: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "park")]
enum ParkKind {
    RateLimit {
        deadline: Timestamp,
    },
    Overloaded {
        /// The non-clocked turn-error marker timestamp. The first retry is
        /// measured from this marker, so a late-observed park can fire
        /// immediately.
        overloaded_at: Timestamp,
    },
    Budget {
        deadline: Timestamp,
    },
}

/// How a parked root agent's turn may resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResumeArm {
    RateLimit { deadline: Timestamp },
    Overloaded { overloaded_at: Timestamp },
}

/// Pure provider-park decision from normalized agent state and account capacity.
pub(crate) fn resume_park(
    agent: &AgentState,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) -> Option<ResumeArm> {
    if agent.is_provider_subagent() || agent.agent_id.is_empty() {
        return None;
    }
    let error = display_turn_error(
        agent.status,
        agent.context.as_ref(),
        agent.last_activity,
        agent.turn_started_at,
    )?;
    match effective_turn_error_class(error) {
        TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit => {
            let deadline = capacity?.latest_spent_window_reset(now)?;
            Some(ResumeArm::RateLimit { deadline })
        }
        TurnErrorClass::PausedOverloaded => Some(ResumeArm::Overloaded {
            overloaded_at: error.at,
        }),
        TurnErrorClass::Unknown | TurnErrorClass::Failed => None,
    }
}

/// Whether a hidden resume-gated message may enter a paused agent now.
pub(crate) fn resume_gate_recovered(
    runtime: &RuntimePaths,
    agent: &AgentState,
    now: Timestamp,
) -> bool {
    if agent.effective_status() != crate::agents::AgentStatus::Paused {
        return false;
    }
    if let Some(park) = agent.budget_park.as_ref() {
        return park.resets_at.is_some_and(|resets_at| now >= resets_at);
    }
    let capacity = ProviderCapacity::read(runtime, agent.kind.as_str());
    match resume_park(agent, capacity.as_ref(), now) {
        Some(ResumeArm::Overloaded { .. }) => true,
        Some(ResumeArm::RateLimit { .. }) => false,
        None => display_turn_error(
            agent.status,
            agent.context.as_ref(),
            agent.last_activity,
            agent.turn_started_at,
        )
        .map(effective_turn_error_class)
        .is_some_and(|class| {
            class.is_limit()
                && capacity.is_some_and(|capacity| capacity.subscription_budget_available(now))
        }),
    }
}

/// Arm or fire each park when live auto-continue is enabled. Best-effort: an
/// empty nudge text or an agent with no live pane waits without consuming a
/// retry; a spawned helper paces the next attempt even if it dies before
/// queueing. Producer-only —
/// one elected producer drives one room, and the records live in that room's
/// runtime dir, so one due condition nudges its agent once per retry.
pub(crate) fn resume_parked(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    resume_messages: &[ResumeMessage],
) {
    if !config.auto_continue {
        return;
    }
    let text = config.auto_continue_text.trim();
    let now = snapshot.now;
    let provider_capacities = ProviderCapacity::read_all(runtime);
    let ctx = FireContext {
        snapshot,
        runtime,
        now,
        text,
        config,
        resume_messages,
    };
    for agent in &snapshot.agents {
        if agent.is_provider_subagent() || agent.agent_id.is_empty() {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        if let Some(park) = agent.budget_park.as_ref() {
            if let Some(deadline) = park.resets_at {
                arm_park(&path, ParkKind::Budget { deadline }, agent.last_activity);
                fire_if_due(agent, &path, ctx);
            }
            continue;
        }
        clear_budget_park(runtime, &agent.kind, &agent.agent_id);
        let capacity = provider_capacities.get(&agent.kind);
        match resume_park(agent, capacity, now) {
            Some(ResumeArm::RateLimit { deadline }) => {
                arm_park(&path, ParkKind::RateLimit { deadline }, agent.last_activity);
                fire_if_due(agent, &path, ctx);
            }
            Some(ResumeArm::Overloaded { overloaded_at }) => {
                arm_park(
                    &path,
                    ParkKind::Overloaded { overloaded_at },
                    agent.last_activity,
                );
                fire_if_due(agent, &path, ctx);
            }
            _ => {
                // No arm this frame means "no recovering window", not "forget
                // the durable deadline". A still-active limit marker gets one
                // chance to fire the persisted due record on the recovery
                // frame; clear only stale records whose marker already moved on.
                if limit_marker_active(agent) {
                    if read_park(&path).is_none() && capacity_recovered(capacity, now) {
                        arm_park(
                            &path,
                            ParkKind::RateLimit { deadline: now },
                            agent.last_activity,
                        );
                    }
                    fire_if_due(agent, &path, ctx);
                } else if capacity_recovered(capacity, now) {
                    remove_park(&path);
                } else {
                    fire_if_due(agent, &path, ctx);
                }
            }
        }
    }
}

pub(crate) fn exhausted_parks(
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    resume_messages: &[ResumeMessage],
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let mut exhausted = BTreeSet::new();
    if !config.auto_continue {
        return exhausted;
    }
    for agent in &snapshot.agents {
        if agent.is_provider_subagent() || agent.agent_id.is_empty() {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        let Some(record) = read_park(&path) else {
            continue;
        };
        if !still_parked(&record, agent.last_activity) {
            continue;
        }
        if latest_resume_message(resume_messages, agent, &record)
            .is_some_and(|message| message.status == MessageStatus::Delivered)
        {
            continue;
        }
        if evidenced_attempts(resume_messages, agent, &record) >= config.auto_continue_max_retries {
            exhausted.insert((agent.kind.clone(), agent.agent_id.clone()));
        }
    }
    exhausted
}

fn capacity_recovered(capacity: Option<&ProviderCapacity>, now: Timestamp) -> bool {
    capacity.is_some_and(|capacity| capacity.subscription_budget_available(now))
}

fn limit_marker_active(agent: &AgentState) -> bool {
    display_turn_error(
        agent.status,
        agent.context.as_ref(),
        agent.last_activity,
        agent.turn_started_at,
    )
    .map(effective_turn_error_class)
    .is_some_and(TurnErrorClass::is_limit)
}

/// Capture (or refresh) the park while the reading is still active. A new park
/// baseline — the first arm, the agent acted and re-parked, or the park class
/// changed — starts a fresh nudge throttle and retry count; a steady or
/// regressed park keeps both and preserves the prior activity baseline.
/// Write-if-changed, so a frozen park costs one write, not one per frame.
fn arm_park(path: &Path, kind: ParkKind, last_activity: Timestamp) {
    let prior = read_park(path);
    let carry = prior
        .as_ref()
        .filter(|record| {
            record.parked_at_activity >= last_activity && same_park_class(&record.kind, &kind)
        })
        .map(|record| {
            (
                record.parked_at_activity,
                record.last_nudge_at,
                record.retries,
            )
        });
    let (parked_at_activity, last_nudge_at, retries) = carry.unwrap_or((last_activity, None, 0));
    let next = ParkRecord {
        kind,
        parked_at_activity,
        last_nudge_at,
        retries,
    };
    if prior.as_ref() != Some(&next) {
        write_park(path, &next);
    }
}

fn same_park_class(left: &ParkKind, right: &ParkKind) -> bool {
    matches!(
        (left, right),
        (ParkKind::RateLimit { .. }, ParkKind::RateLimit { .. })
            | (ParkKind::Overloaded { .. }, ParkKind::Overloaded { .. })
            | (ParkKind::Budget { .. }, ParkKind::Budget { .. })
    )
}

#[derive(Clone, Copy)]
struct FireContext<'a> {
    snapshot: &'a SidebarSnapshot,
    runtime: &'a RuntimePaths,
    now: Timestamp,
    text: &'a str,
    config: &'a ResumeConfig,
    resume_messages: &'a [ResumeMessage],
}

/// Fire a parked agent's resume when its recorded condition is due and it is
/// still idle. A woken agent (activity advanced) clears the record; a pane that
/// has not appeared yet, a condition still ahead, or a recent nudge each waits.
fn fire_if_due(agent: &AgentState, path: &Path, ctx: FireContext<'_>) {
    let Some(record) = read_park(path) else {
        return;
    };
    if !still_parked(&record, agent.last_activity) {
        remove_park(path);
        return;
    }
    let retry_message_id =
        if let Some(message) = latest_resume_message(ctx.resume_messages, agent, &record) {
            match message.status {
                MessageStatus::Delivered => {
                    remove_park(path);
                    return;
                }
                MessageStatus::Queued | MessageStatus::Claimed => Some(message.message_id.clone()),
                MessageStatus::Sent
                | MessageStatus::TimedOut
                | MessageStatus::Errored
                | MessageStatus::Canceled
                | MessageStatus::Abandoned
                | MessageStatus::Archived => None,
            }
        } else {
            None
        };
    let reason = match &record.kind {
        ParkKind::RateLimit { .. } => "rate_limit_window_reset",
        ParkKind::Overloaded { .. } => "overloaded_backoff_retry",
        ParkKind::Budget { .. } => "budget_day_reset",
    };
    if ctx.text.is_empty() {
        return;
    }
    let attempts = evidenced_attempts(ctx.resume_messages, agent, &record);
    if !nudge_due(
        &record,
        attempts,
        ctx.now,
        &ctx.config.auto_continue_backoff_secs,
        ctx.config.auto_continue_max_retries,
    ) {
        return;
    }
    let Some(pane_id) = ctx.snapshot.live_agent_pane(&agent.kind, &agent.agent_id) else {
        return;
    };
    let peers = crate::harness::target::addressable_agents(ctx.snapshot);
    let label = crate::harness::target::agent_handle(agent, &peers, false);
    if !spawn_auto_continue(
        ctx.runtime,
        &agent.kind,
        &agent.agent_id,
        &pane_id,
        retry_message_id.as_ref(),
        ctx.text,
        AutoContinueFacts {
            reason,
            parked_since: record.parked_at_activity,
            label: Some(&label),
        },
    ) {
        return;
    }
    write_park(path, &nudged_record(record, ctx.now));
}

struct AutoContinueFacts<'a> {
    reason: &'a str,
    parked_since: Timestamp,
    label: Option<&'a str>,
}

/// Whether the agent has done nothing since the park was armed. Equal or
/// regressed rollup activity is the same park; only activity past the captured
/// baseline means it woke (our nudge took, or it resumed on its own), so the
/// record is stale.
fn still_parked(record: &ParkRecord, last_activity: Timestamp) -> bool {
    last_activity <= record.parked_at_activity
}

fn nudged_record(mut record: ParkRecord, now: Timestamp) -> ParkRecord {
    record.last_nudge_at = Some(now);
    record.retries += 1;
    record
}

fn overload_backoff(retries: u32, backoff_secs: &[u64]) -> Duration {
    let idx = (retries as usize).min(backoff_secs.len().saturating_sub(1));
    let fallback = DEFAULT_AUTO_CONTINUE_BACKOFF_SECS
        .last()
        .copied()
        .unwrap_or(180);
    Duration::from_secs(backoff_secs.get(idx).copied().unwrap_or(fallback))
}

/// Whether a nudge is due for this park class. Rate limits wait for the captured
/// deadline and then throttle repeats; backoff records wait from park time for
/// the first try, then from the prior nudge for each retry step until the
/// evidenced-attempt cap.
fn nudge_due(
    record: &ParkRecord,
    attempts: u32,
    now: Timestamp,
    backoff_secs: &[u64],
    max_retries: u32,
) -> bool {
    match &record.kind {
        ParkKind::RateLimit { deadline } | ParkKind::Budget { deadline } => {
            if attempts >= max_retries {
                return false;
            }
            now >= *deadline
                && record.last_nudge_at.is_none_or(|at| {
                    now.as_second() - at.as_second()
                        >= AUTO_CONTINUE_RETRY_INTERVAL.as_secs() as i64
                })
        }
        ParkKind::Overloaded { overloaded_at } => {
            if attempts >= max_retries {
                return false;
            }
            let anchor = record.last_nudge_at.unwrap_or(*overloaded_at);
            now.as_second() - anchor.as_second()
                >= overload_backoff(record.retries, backoff_secs).as_secs() as i64
        }
    }
}

fn evidenced_attempts(messages: &[ResumeMessage], agent: &AgentState, record: &ParkRecord) -> u32 {
    let count = messages
        .iter()
        .filter(|message| {
            message.same_agent_card(agent) && message.enqueued_at >= record.parked_at_activity
        })
        .map(|message| message.message_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    count.min(u32::MAX as usize) as u32
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ResumeMessage {
    message_id: MessageId,
    kind: AgentKind,
    agent_id: AgentSessionId,
    agent_name: Option<String>,
    status: MessageStatus,
    enqueued_at: Timestamp,
    updated_at: Timestamp,
}

impl ResumeMessage {
    fn from_record(message: &MessageRecord) -> Option<Self> {
        (message.gate == DeliveryGate::Resume && message.body == MessageBody::Prompt).then(|| {
            Self {
                message_id: message.message_id.clone(),
                kind: message.kind.clone(),
                agent_id: message.agent_id.clone(),
                agent_name: message.agent_name.clone(),
                status: message.status,
                enqueued_at: message.enqueued_at,
                updated_at: message.updated_at,
            }
        })
    }

    fn from_outcome(outcome: &ResumeOutcome) -> Self {
        Self {
            message_id: outcome.message_id.clone(),
            kind: outcome.kind.clone(),
            agent_id: outcome.agent_id.clone(),
            agent_name: outcome.agent_name.clone(),
            status: outcome.status,
            enqueued_at: outcome.enqueued_at,
            updated_at: outcome.updated_at,
        }
    }

    fn same_agent_card(&self, agent: &AgentState) -> bool {
        AgentCardRef::new(&self.kind, &self.agent_id, self.agent_name.as_deref())
            .matches(agent.card_ref())
    }
}

pub(crate) fn read_resume_messages(
    messages_dir: Option<&Path>,
    outcomes: &[ResumeOutcome],
) -> Vec<ResumeMessage> {
    let mut messages = outcomes
        .iter()
        .map(ResumeMessage::from_outcome)
        .collect::<Vec<_>>();
    let Some(messages_dir) = messages_dir else {
        return messages;
    };
    messages.extend(
        crate::store::message_store::list(messages_dir)
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(ResumeMessage::from_record)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    messages
}

fn latest_resume_message<'a>(
    messages: &'a [ResumeMessage],
    agent: &AgentState,
    record: &ParkRecord,
) -> Option<&'a ResumeMessage> {
    messages
        .iter()
        .filter(|message| {
            message.same_agent_card(agent) && message.enqueued_at >= record.parked_at_activity
        })
        .max_by(|left, right| {
            left.enqueued_at
                .cmp(&right.enqueued_at)
                .then_with(|| left.updated_at.cmp(&right.updated_at))
                .then_with(|| left.message_id.as_str().cmp(right.message_id.as_str()))
        })
}

fn read_park(path: &Path) -> Option<ParkRecord> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_park(path: &Path, record: &ParkRecord) {
    if let Err(err) = write_temp_then_rename_cache(path, record) {
        tracing::warn!(
            tags.operation = "auto_continue.write_park",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to record resumable park",
        );
    }
}

fn remove_park(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn park_record_path(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> PathBuf {
    runtime.root.join(format!(
        "auto-continue.{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

pub(crate) fn arm_budget_park(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    deadline: Timestamp,
    last_activity: Timestamp,
) {
    arm_park(
        &park_record_path(runtime, kind, agent_id),
        ParkKind::Budget { deadline },
        last_activity,
    );
}

pub fn clear_budget_park(runtime: &RuntimePaths, kind: &AgentKind, agent_id: &AgentSessionId) {
    let path = park_record_path(runtime, kind, agent_id);
    if read_park(&path).is_some_and(|record| matches!(record.kind, ParkKind::Budget { .. })) {
        remove_park(&path);
    }
}

#[cfg(test)]
pub(crate) fn budget_park_armed(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> bool {
    read_park(&park_record_path(runtime, kind, agent_id))
        .is_some_and(|record| matches!(record.kind, ParkKind::Budget { .. }))
}

/// Spawn the detached, fresh-stdio helper that queues or redelivers the
/// resume-gated message. Best-effort: a spawn failure is logged without
/// consuming an attempt; a spawned helper that dies before queueing is still
/// paced by the durable nudge stamp.
fn spawn_auto_continue(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    pane_id: &PaneId,
    message_id: Option<&MessageId>,
    text: &str,
    facts: AutoContinueFacts<'_>,
) -> bool {
    let request = AutoContinueRequest {
        workspace_id: runtime.workspace_id.clone(),
        kind: kind.clone(),
        agent_id: agent_id.clone(),
        pane_id: pane_id.clone(),
        message_id: message_id.cloned(),
        parked_since: facts.parked_since,
        text: text.to_owned(),
        reason: facts.reason.to_owned(),
        label: facts.label.map(str::to_owned),
    };
    let args = crate::child_process::agent_helper_argv("auto-continue", &request);
    tracing::info!(
        target: crate::observability::BREADCRUMB_TARGET,
        workspace = %runtime.workspace_id,
        kind = %kind,
        reason = facts.reason,
        "sidebar: auto-continuing parked agent",
    );
    if let Err(err) =
        crate::child_process::spawn_detached_rimz(runtime, args, "agent-auto-continue")
    {
        // Best-effort enrichment on a throttled producer path. The CWD anchor
        // clears the gc'd-worktree ENOENT; a bad RIMZ_BIN/PATH is an
        // environment fact, not a RimZ fault. Keep it at debug! so it never
        // reaches Sentry.
        tracing::debug!(
            workspace = %runtime.workspace_id,
            tags.operation = "auto_continue.spawn",
            error = &err as &dyn std::error::Error,
            "sidebar: failed to spawn agent auto-continue",
        );
        return false;
    }
    true
}

#[cfg(test)]
mod tests;
