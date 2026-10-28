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
//! - **Arm.** On each due agent check with a resumable park certificate
//!   ([`resume_park`]), the producer writes a durable [`ParkRecord`]
//!   capturing the park class and the agent's frozen `last_activity`. A
//!   rate-limit record captures the latest spent-window reset deadline; a
//!   backoff record carries the turn-error marker time and retry state.
//! - **Fire.** Once the window reset deadline or retry backoff is due and the
//!   agent is still idle (`last_activity` has not advanced), the producer
//!   spawns the detached `rimz agents auto-continue` helper that queues and
//!   delivers a gated message record (Done for budget resets, Resume otherwise).
//! - **Clear.** A rate-limit or overload record whose activity advanced is held while the reply is unclassified (Running or Failed with no displayed error), even before the resume message becomes visible. With a matching resume message, no root tool timestamp strictly after resume enqueue and at or after the provider turn start (when known) identifies a bare reply. A tool reopening the turn may equal its start, but an enqueue-time tie does not prove post-nudge work. A same-class limit or overload reply then carries the activity baseline forward, preserving the attempt anchor and retry state. An observed tool meeting those boundaries earns a fresh allowance even if another limit follows; text-only work still counts as a reply. A settled status with no marker or a delivered message with no turn start clears the record. Every retained terminal outcome and the live resume queue evidence exhaustion, while helper spawns only pace retries.
//!
//! This module owns only the durable record, the pane join, and the spawn — the
//! arm decision is the pure, unit-tested [`resume_park`].
//! The caller owns per-agent deferral. Projection and no-op arm/fire probes
//! reuse stamped park parses. A write, removal, or helper spawn rereads fresh
//! bytes and recomputes its decision first.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::RuntimePaths;
use crate::agents::{AgentCardRef, AgentState, AgentStatus, ProviderCapacity, TurnErrorClass};
use crate::config::ResumeConfig;
use crate::disk::atomic::write_temp_then_rename_cache;
use crate::disk::parse_cache::KeyedParseCache;
use crate::ids::{AgentKind, AgentSessionId, MessageId, PaneId, WorkspaceId};
use crate::store::message::{DeliveryGate, MessageBody, MessageRecord, MessageStatus};
#[cfg(test)]
use crate::store::snapshot::PaneAgent;
use crate::store::snapshot::ResumeOutcome;
use crate::store::snapshot::SidebarSnapshot;

/// Minimum gap between auto-continue nudges to one rate-limit-parked agent. One
/// nudge resumes the turn within a frame, so this mostly bounds the brief window
/// before the agent's first hook lands; if a nudge fails to wake a still-parked
/// agent, RimZ retries on this cadence rather than typing every frame.
const AUTO_CONTINUE_RETRY_INTERVAL: Duration = Duration::from_secs(120);

/// Read durable park evidence for the sidebar and rowless CLI projections.
///
/// Resolves room logins, reads cached capacity and park records, and merges every retained terminal resume outcome with the live message queue. No store write or subprocess is needed, including before the room has a pane frame.
pub fn park_demotion(
    store: Option<&crate::Store>,
    state: &crate::StatePaths,
    runtime: &RuntimePaths,
    config: &crate::config::MachineConfig,
    agents: &[AgentState],
    outcomes: &[ResumeOutcome],
    now: Timestamp,
) -> crate::agents::ParkDemotion {
    let logins = crate::agents::RoomLoginSet::resolve(&state.workspace_record, None, config)
        .with_agents(agents);
    park_demotion_for_logins(
        |_| false,
        store,
        runtime,
        &config.resume,
        &logins,
        agents,
        outcomes,
        now,
    )
}

/// [`park_demotion`] for a caller that already resolved the room's logins.
#[expect(
    clippy::too_many_arguments,
    reason = "the read boundary also carries its caller's per-agent hold decision"
)]
pub(crate) fn park_demotion_for_logins(
    deferred: impl FnMut(&AgentState) -> bool,
    store: Option<&crate::Store>,
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    logins: &crate::agents::RoomLoginSet,
    agents: &[AgentState],
    outcomes: &[ResumeOutcome],
    now: Timestamp,
) -> crate::agents::ParkDemotion {
    let capacities = ProviderCapacity::read_all(runtime, logins);
    let messages = read_resume_messages(store, config, outcomes);
    crate::agents::ParkDemotion::new(
        &capacities,
        exhausted_parks(deferred, agents, runtime, config, &messages),
        now,
    )
}

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

impl AutoContinueRequest {
    pub fn delivery_gate(&self) -> DeliveryGate {
        match self.reason.as_str() {
            "budget_day_reset" | "budget_month_reset" => DeliveryGate::Done,
            _ => DeliveryGate::Resume,
        }
    }
}

/// A durable record of one park: written while the park is fresh, read after its
/// class-specific resume condition is due. It outlives the per-session context
/// the park was first seen through, so a resume survives both an expired context
/// sidecar and a fresh non-spent reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ParkRecord {
    /// The park class and its durable resume facts.
    kind: ParkKind,
    /// Current activity baseline; a nudge's bare limit or overload reply advances it without starting a new park.
    parked_at_activity: Timestamp,
    /// Original baseline when a bare limit or overload reply rebases the activity stamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempts_since: Option<Timestamp>,
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
    BudgetMonth {
        deadline: Timestamp,
    },
}

impl ParkKind {
    fn reason(&self) -> &'static str {
        match self {
            Self::RateLimit { .. } => "rate_limit_window_reset",
            Self::Overloaded { .. } => "overloaded_backoff_retry",
            Self::Budget { .. } => "budget_day_reset",
            Self::BudgetMonth { .. } => "budget_month_reset",
        }
    }

    fn budget(deadline: Timestamp, window: crate::agents::BudgetWindow) -> Self {
        if window == crate::agents::BudgetWindow::Month {
            Self::BudgetMonth { deadline }
        } else {
            Self::Budget { deadline }
        }
    }
}

/// How a parked root agent's turn may resume.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ResumeArm {
    RateLimit { deadline: Timestamp },
    Overloaded { overloaded_at: Timestamp },
}

/// Pure provider-park decision from normalized agent state and account capacity.
fn resume_park(
    agent: &AgentState,
    capacity: Option<&ProviderCapacity>,
    now: Timestamp,
) -> Option<ResumeArm> {
    if agent.is_provider_subagent() || agent.agent_id.is_empty() {
        return None;
    }
    let (class, error) = agent.displayed_turn_error()?;
    match class {
        TurnErrorClass::PausedRateLimit | TurnErrorClass::PausedSpendLimit => {
            let deadline =
                capacity?.latest_spent_window_reset_for_model(now, agent.model.as_deref())?;
            Some(ResumeArm::RateLimit { deadline })
        }
        TurnErrorClass::PausedOverloaded => Some(ResumeArm::Overloaded {
            overloaded_at: error.at,
        }),
        TurnErrorClass::Unknown | TurnErrorClass::Failed | TurnErrorClass::NotEntitled => None,
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
    let capacity = ProviderCapacity::read(runtime, &agent.login_key());
    match resume_park(agent, capacity.as_ref(), now) {
        Some(ResumeArm::Overloaded { .. }) => true,
        Some(ResumeArm::RateLimit { .. }) => false,
        None => limit_marker_active(agent) && capacity_recovered(capacity.as_ref(), agent, now),
    }
}

/// Arm or fire each park when live auto-continue is enabled. Best-effort: an
/// empty nudge text or an agent with no live pane waits without consuming a
/// retry; a spawned helper paces the next attempt even if it dies before
/// queueing. Producer-only —
/// one room host drives one room, and the records live in that room's
/// runtime dir, so one due condition nudges its agent once per retry.
pub(crate) fn resume_parked(
    mut deferred: impl FnMut(&AgentState) -> bool,
    snapshot: &SidebarSnapshot,
    runtime: &RuntimePaths,
    logins: &crate::agents::RoomLoginSet,
    config: &ResumeConfig,
    resume_messages: &[ResumeMessage],
) {
    if !config.auto_continue {
        return;
    }
    let text = config.auto_continue_text.trim();
    let now = snapshot.now;
    let provider_capacities = ProviderCapacity::read_all(runtime, logins);
    let ctx = FireContext {
        snapshot,
        runtime,
        now,
        text,
        config,
        resume_messages,
    };
    for agent in snapshot.agents.iter() {
        if agent.is_provider_subagent() || agent.agent_id.is_empty() {
            continue;
        }
        if deferred(agent) {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        if let Some(park) = agent.budget_park.as_ref() {
            if let Some(deadline) = park.resets_at {
                arm_park(
                    &path,
                    ParkKind::budget(deadline, park.reset_window.unwrap_or(park.window)),
                    agent.last_activity,
                );
                fire_if_due(agent, &path, ctx);
            }
            continue;
        }
        if cached_park(&path).is_some_and(|record| {
            matches!(
                record.kind,
                ParkKind::Budget { deadline } | ParkKind::BudgetMonth { deadline }
                    if now >= deadline
            )
        }) {
            fire_if_due(agent, &path, ctx);
            continue;
        }
        clear_budget_park(runtime, &agent.kind, &agent.agent_id);
        if let Some(record) = cached_park(&path)
            && matches!(
                record.kind,
                ParkKind::RateLimit { .. } | ParkKind::Overloaded { .. }
            )
            && !still_parked(&record, agent.last_activity)
        {
            let turn_error = agent.displayed_turn_error();
            if matches!(agent.status, AgentStatus::Running | AgentStatus::Failed)
                && turn_error.is_none()
            {
                continue;
            }
            if rebased_record(record, agent, resume_messages).is_some()
                && let Some(record) = read_park(&path)
                    .and_then(|record| rebased_record(record, agent, resume_messages))
            {
                write_park(&path, &record);
            }
        }
        let capacity = provider_capacities.get(&agent.login_key());
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
                    if cached_park(&path).is_none()
                        && capacity_recovered(capacity, agent, now)
                        && !recent_resume_attempt(resume_messages, agent, now)
                    {
                        arm_park(
                            &path,
                            ParkKind::RateLimit { deadline: now },
                            agent.last_activity,
                        );
                    }
                    fire_if_due(agent, &path, ctx);
                } else if capacity_recovered(capacity, agent, now) {
                    if cached_park(&path).is_some() && read_park(&path).is_some() {
                        remove_park(&path);
                    }
                } else {
                    fire_if_due(agent, &path, ctx);
                }
            }
        }
    }
}

fn rebased_record(
    mut record: ParkRecord,
    agent: &AgentState,
    resume_messages: &[ResumeMessage],
) -> Option<ParkRecord> {
    if !matches!(
        record.kind,
        ParkKind::RateLimit { .. } | ParkKind::Overloaded { .. }
    ) || still_parked(&record, agent.last_activity)
        || !agent
            .displayed_turn_error()
            .is_some_and(|(class, _)| class.is_limit() || class == TurnErrorClass::PausedOverloaded)
    {
        return None;
    }
    let message = latest_resume_message(resume_messages, agent, &record)?;
    if !nudged_turn_made_no_progress(agent, message.enqueued_at) {
        return None;
    }
    record
        .attempts_since
        .get_or_insert(record.parked_at_activity);
    record.parked_at_activity = agent.last_activity;
    Some(record)
}

fn nudged_turn_made_no_progress(agent: &AgentState, enqueued_at: Timestamp) -> bool {
    // A tool hook can itself reopen the turn at the same ingress timestamp.
    let started_at = agent.turn_started_at.unwrap_or(enqueued_at);
    agent
        .last_tool_at
        .is_none_or(|tool_at| tool_at <= enqueued_at || tool_at < started_at)
}

pub(crate) fn exhausted_parks(
    mut deferred: impl FnMut(&AgentState) -> bool,
    agents: &[AgentState],
    runtime: &RuntimePaths,
    config: &ResumeConfig,
    resume_messages: &[ResumeMessage],
) -> BTreeSet<(AgentKind, AgentSessionId)> {
    let mut exhausted = BTreeSet::new();
    if !config.auto_continue {
        PROJECTED_PARKS.with_borrow_mut(BTreeMap::clear);
        return exhausted;
    }
    let needed: BTreeSet<_> = agents
        .iter()
        .filter(|agent| !agent.is_provider_subagent() && !agent.agent_id.is_empty())
        .map(|agent| park_record_path(runtime, &agent.kind, &agent.agent_id))
        .collect();
    PROJECTED_PARKS.with_borrow_mut(|memo| memo.retain(|path, _| needed.contains(path)));
    for agent in agents {
        if agent.is_provider_subagent() || agent.agent_id.is_empty() {
            continue;
        }
        let path = park_record_path(runtime, &agent.kind, &agent.agent_id);
        let Some(record) = projected_park(&path, deferred(agent)) else {
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

thread_local! {
    static PROJECTED_PARKS: RefCell<BTreeMap<PathBuf, Option<ParkRecord>>> = RefCell::default();
    static PARSED_PARKS: KeyedParseCache<ParkRecord> = KeyedParseCache::default();
}

fn projected_park(path: &Path, deferred: bool) -> Option<ParkRecord> {
    PROJECTED_PARKS.with_borrow_mut(|memo| {
        if deferred {
            return memo.get(path).cloned().flatten();
        }
        let raw = cached_park(path);
        memo.insert(path.to_owned(), raw.clone());
        raw
    })
}

fn capacity_recovered(
    capacity: Option<&ProviderCapacity>,
    agent: &AgentState,
    now: Timestamp,
) -> bool {
    capacity.is_some_and(|capacity| {
        capacity.subscription_budget_available_for_model(now, agent.model.as_deref())
    })
}

/// Whether the row's displayed turn error is a provider limit: the one
/// definition of "stopped on a limit" auto-continue and park-notice share.
pub(crate) fn limit_marker_active(agent: &AgentState) -> bool {
    agent
        .displayed_turn_error()
        .is_some_and(|(class, _)| class.is_limit())
}

/// Capture (or refresh) the park while the reading is still active. A new park
/// baseline — the first arm, the agent acted and re-parked, or the park class
/// changed — starts a fresh nudge throttle and retry count; a steady or
/// regressed park keeps both and preserves the prior activity baseline.
/// Write-if-changed, so a frozen park costs one write, not one per frame.
fn arm_park(path: &Path, kind: ParkKind, last_activity: Timestamp) {
    let prior = cached_park(path);
    if prior.as_ref() == Some(&armed_record(prior.as_ref(), kind.clone(), last_activity)) {
        return;
    }
    let prior = read_park(path);
    let next = armed_record(prior.as_ref(), kind, last_activity);
    if prior.as_ref() != Some(&next) {
        write_park(path, &next);
    }
}

fn armed_record(
    prior: Option<&ParkRecord>,
    kind: ParkKind,
    last_activity: Timestamp,
) -> ParkRecord {
    let carry = prior
        .filter(|record| {
            record.parked_at_activity >= last_activity && same_park_class(&record.kind, &kind)
        })
        .map(|record| {
            (
                record.parked_at_activity,
                record.attempts_since,
                record.last_nudge_at,
                record.retries,
            )
        });
    let (parked_at_activity, attempts_since, last_nudge_at, retries) =
        carry.unwrap_or((last_activity, None, None, 0));
    ParkRecord {
        kind,
        parked_at_activity,
        attempts_since,
        last_nudge_at,
        retries,
    }
}

fn same_park_class(left: &ParkKind, right: &ParkKind) -> bool {
    matches!(
        (left, right),
        (ParkKind::RateLimit { .. }, ParkKind::RateLimit { .. })
            | (ParkKind::Overloaded { .. }, ParkKind::Overloaded { .. })
            | (
                ParkKind::Budget { .. } | ParkKind::BudgetMonth { .. },
                ParkKind::Budget { .. } | ParkKind::BudgetMonth { .. }
            )
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
    if cached_park(path).is_none_or(|record| park_action(&record, agent, ctx).is_none()) {
        return;
    }
    let Some(record) = read_park(path) else {
        return;
    };
    let Some(action) = park_action(&record, agent, ctx) else {
        return;
    };
    let ParkAction::Nudge {
        pane_id,
        retry_message_id,
    } = action
    else {
        remove_park(path);
        return;
    };
    let peers = crate::address::addressable_agents(ctx.snapshot);
    let label = crate::address::agent_handle(agent, &peers, false);
    if !spawn_auto_continue(
        ctx.runtime,
        &agent.kind,
        &agent.agent_id,
        &pane_id,
        retry_message_id.as_ref(),
        ctx.text,
        AutoContinueFacts {
            reason: record.kind.reason(),
            parked_since: record.parked_at_activity,
            label: Some(&label),
        },
    ) {
        return;
    }
    write_park(path, &nudged_record(record, ctx.now));
}

enum ParkAction {
    Clear,
    Nudge {
        pane_id: PaneId,
        retry_message_id: Option<MessageId>,
    },
}

fn park_action(
    record: &ParkRecord,
    agent: &AgentState,
    ctx: FireContext<'_>,
) -> Option<ParkAction> {
    if !still_parked(record, agent.last_activity) {
        return Some(ParkAction::Clear);
    }
    let retry_message_id =
        if let Some(message) = latest_resume_message(ctx.resume_messages, agent, record) {
            match message.status {
                MessageStatus::Delivered => {
                    return Some(ParkAction::Clear);
                }
                MessageStatus::Queued | MessageStatus::Claimed => Some(message.message_id.clone()),
                MessageStatus::Sent
                | MessageStatus::TimedOut
                | MessageStatus::Errored
                | MessageStatus::Canceled
                | MessageStatus::Abandoned
                | MessageStatus::Expired
                | MessageStatus::Archived => None,
            }
        } else {
            None
        };
    if ctx.text.is_empty() {
        return None;
    }
    if matches!(record.kind, ParkKind::RateLimit { .. })
        && recent_resume_attempt(ctx.resume_messages, agent, ctx.now)
    {
        return None;
    }
    let attempts = evidenced_attempts(ctx.resume_messages, agent, record);
    if !nudge_due(record, attempts, ctx.now, ctx.config) {
        return None;
    }
    Some(ParkAction::Nudge {
        pane_id: ctx.snapshot.live_agent_pane(&agent.kind, &agent.agent_id)?,
        retry_message_id,
    })
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

/// Whether a nudge is due for this park class. Rate limits wait for the captured
/// deadline and then throttle repeats; backoff records wait from park time for
/// the first try, then from the prior nudge for each retry step until the
/// evidenced-attempt cap.
fn nudge_due(record: &ParkRecord, attempts: u32, now: Timestamp, config: &ResumeConfig) -> bool {
    match &record.kind {
        ParkKind::RateLimit { deadline }
        | ParkKind::Budget { deadline }
        | ParkKind::BudgetMonth { deadline } => {
            if attempts >= config.auto_continue_max_retries {
                return false;
            }
            now >= *deadline
                && record.last_nudge_at.is_none_or(|at| {
                    now.as_second() - at.as_second()
                        >= AUTO_CONTINUE_RETRY_INTERVAL.as_secs() as i64
                })
        }
        ParkKind::Overloaded { overloaded_at } => {
            if attempts >= config.auto_continue_max_retries {
                return false;
            }
            let anchor = record.last_nudge_at.unwrap_or(*overloaded_at);
            now.as_second() - anchor.as_second()
                >= config.auto_continue_backoff(record.retries).as_secs() as i64
        }
    }
}

fn evidenced_attempts(messages: &[ResumeMessage], agent: &AgentState, record: &ParkRecord) -> u32 {
    let since = record.attempts_since.unwrap_or(record.parked_at_activity);
    let count = messages
        .iter()
        .filter(|message| message.same_agent_card(agent) && message.enqueued_at >= since)
        .map(|message| message.message_id.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    count.min(u32::MAX as usize) as u32
}

fn recent_resume_attempt(messages: &[ResumeMessage], agent: &AgentState, now: Timestamp) -> bool {
    messages.iter().any(|message| {
        message.same_agent_card(agent)
            && now.duration_since(message.enqueued_at)
                < jiff::SignedDuration::from_secs(AUTO_CONTINUE_RETRY_INTERVAL.as_secs() as i64)
    })
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

/// Merge every retained terminal resume outcome with the live resume prompt queue.
pub(crate) fn read_resume_messages(
    store: Option<&crate::store::Store>,
    config: &ResumeConfig,
    outcomes: &[ResumeOutcome],
) -> Vec<ResumeMessage> {
    if !config.auto_continue {
        return Vec::new();
    }
    let mut messages = outcomes
        .iter()
        .map(ResumeMessage::from_outcome)
        .collect::<Vec<_>>();
    let Some(store) = store else {
        return messages;
    };
    messages.extend(
        crate::store::message::cached_messages(store.paths(), false)
            .iter()
            .filter_map(|record| ResumeMessage::from_record(record)),
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
    #[cfg(any(test, feature = "testkit"))]
    crate::testkit::host_pass::count(crate::testkit::host_pass::Counter::ParkRead);
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn cached_park(path: &Path) -> Option<ParkRecord> {
    PARSED_PARKS.with(|cache| {
        cache
            .read_json(path)
            .ok()
            .flatten()
            .map(|value| (*value).clone())
    })
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
    runtime.lane_path(format!(
        "auto-continue.{}.json",
        crate::store::sidecar::digest(kind.as_str(), agent_id.as_str())
    ))
}

pub(super) fn arm_budget_park(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
    deadline: Timestamp,
    last_activity: Timestamp,
    window: crate::agents::BudgetWindow,
) {
    arm_park(
        &park_record_path(runtime, kind, agent_id),
        ParkKind::budget(deadline, window),
        last_activity,
    );
}

pub fn clear_budget_park(runtime: &RuntimePaths, kind: &AgentKind, agent_id: &AgentSessionId) {
    let path = park_record_path(runtime, kind, agent_id);
    if !cached_park(&path).is_some_and(|record| {
        matches!(
            record.kind,
            ParkKind::Budget { .. } | ParkKind::BudgetMonth { .. }
        )
    }) {
        return;
    }
    if read_park(&path).is_some_and(|record| {
        matches!(
            record.kind,
            ParkKind::Budget { .. } | ParkKind::BudgetMonth { .. }
        )
    }) {
        remove_park(&path);
    }
}

#[cfg(test)]
pub(crate) fn budget_park_armed(
    runtime: &RuntimePaths,
    kind: &AgentKind,
    agent_id: &AgentSessionId,
) -> bool {
    read_park(&park_record_path(runtime, kind, agent_id)).is_some_and(|record| {
        matches!(
            record.kind,
            ParkKind::Budget { .. } | ParkKind::BudgetMonth { .. }
        )
    })
}

/// Spawn the detached, fresh-stdio helper that queues or redelivers the
/// gated message. Best-effort: a spawn failure is logged without
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
