//! Synchronous reply preparation and wait aggregation.
//!
//! This module owns reply target validation, transcript anchoring, wait-leg
//! transitions, dependency-cycle handling, timeout mutation, transcript
//! extraction, and join settlement. Callers drive polling and render the typed
//! progress and result records.

use std::collections::{BTreeSet, HashSet};
use std::path::PathBuf;

use jiff::Timestamp;

use crate::Store;
use crate::agents::transcript::TranscriptCursor;
use crate::agents::{AgentCardRef, AgentDefinition, AgentState, AgentStatus};
use crate::harness::run::RunStatus;
use crate::ids::{AgentKind, AgentSessionId, MessageId};
use crate::message::{MessageRecord, MessageSender, MessageStatus};
use crate::store::event::EventKind;
use crate::store::event_log;
use crate::store::snapshot::SidebarSnapshot;

use super::dispatch::DispatchOutcome;

const WAIT_GUARD_TICKS: u8 = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyJoin {
    All,
    Any,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyProgress {
    Target { label: String, parked: bool },
    Fanout { pending: usize, total: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReplyFailure {
    WaitingForInput,
    DeliveryFailed {
        status: MessageStatus,
    },
    AgentGone,
    Deadlock {
        first_handle: Option<String>,
        first_message_id: Option<MessageId>,
        chain: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyResult {
    pub label: String,
    pub message_id: MessageId,
    pub status: RunStatus,
    pub final_message: Option<String>,
    pub failure: Option<ReplyFailure>,
    pub transcript_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyJoinResult {
    pub status: RunStatus,
    pub winner: Option<MessageId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyUpdate {
    pub settled: Vec<ReplyResult>,
    pub join: Option<ReplyJoinResult>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReplyPrepareErr {
    #[error("reply wait requires at least one target")]
    NoTargets,
    #[error("reply wait target `{label}` has no lifecycle state")]
    PaneOnly { label: String },
    #[error("reply wait target `{label}` is not running")]
    NotLive { label: String },
    #[error("unknown agent kind `{0}`")]
    UnknownAgentKind(AgentKind),
    #[error("turn lifecycle is unsupported for `{kind}`: {reason}")]
    TurnLifecycleUnsupported { kind: AgentKind, reason: String },
    #[error("reply wait hooks are missing for `{kind}`")]
    HooksMissing { kind: AgentKind },
    #[error("reply wait hooks are untrusted for `{kind}`: {hooks}")]
    HooksUntrusted {
        kind: AgentKind,
        hooks: String,
        fix: String,
    },
    #[error("reply wait would create a dependency cycle")]
    DependencyCycle {
        target: String,
        first_handle: Option<String>,
        first_message_id: Option<MessageId>,
        chain: Option<String>,
    },
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
}

#[derive(Debug, thiserror::Error)]
pub enum ReplyErr {
    #[error(transparent)]
    Store(#[from] crate::store::StoreErr),
    #[error(transparent)]
    EventLog(#[from] crate::store::event_log::EventLogErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("unknown agent kind `{0}`")]
    UnknownAgentKind(AgentKind),
    #[error(
        "reply preparation produced {targets} targets but dispatch produced {outcomes} outcomes"
    )]
    OutcomeCount { targets: usize, outcomes: usize },
}

pub(super) struct PreparationTarget<'a> {
    pub agent: Option<&'a AgentState>,
    pub label: String,
}

pub(super) struct ReplyPreparation {
    targets: Vec<ReplyTarget>,
    wait_base: u64,
    caller_identity: Option<(AgentKind, String)>,
}

impl ReplyPreparation {
    pub(super) fn new<'a>(
        store: &Store,
        snapshot: &SidebarSnapshot,
        targets: impl IntoIterator<Item = PreparationTarget<'a>>,
        caller_identity: Option<(AgentKind, String)>,
    ) -> Result<Self, ReplyPrepareErr> {
        let targets = targets.into_iter().collect::<Vec<_>>();
        if targets.is_empty() {
            return Err(ReplyPrepareErr::NoTargets);
        }
        let guard_records = if caller_identity.is_some() {
            Some((store.list_messages()?, store.list_message_history()?))
        } else {
            None
        };
        let mut checked_kinds = BTreeSet::new();
        let mut prepared = Vec::with_capacity(targets.len());
        for target in targets {
            let identity = target.agent.ok_or_else(|| ReplyPrepareErr::PaneOnly {
                label: target.label.clone(),
            })?;
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| same_card(identity, agent))
                .ok_or_else(|| ReplyPrepareErr::NotLive {
                    label: target.label.clone(),
                })?;
            let adapter = crate::agents::find_definition(agent.kind.as_str())
                .ok_or_else(|| ReplyPrepareErr::UnknownAgentKind(agent.kind.clone()))?;
            if checked_kinds.insert(agent.kind.clone()) {
                preflight_reply_hooks(agent, adapter)?;
            }
            if let (Some((self_kind, self_name)), Some((live, history))) =
                (caller_identity.as_ref(), guard_records.as_ref())
                && let Some(cycle) =
                    wait_cycle(live, history, &snapshot.agents, self_kind, self_name, agent)
            {
                let first = cycle.first();
                return Err(ReplyPrepareErr::DependencyCycle {
                    target: target.label,
                    first_handle: first.map(|hop| hop.handle.clone()),
                    first_message_id: first.map(|hop| hop.message_id.clone()),
                    chain: render_chain(&cycle),
                });
            }
            prepared.push(ReplyTarget::new(agent, target.label, adapter));
        }
        Ok(Self {
            targets: prepared,
            wait_base: store.wait_fold_base()?,
            caller_identity,
        })
    }

    pub(super) fn attach(
        self,
        outcomes: &[DispatchOutcome],
        steer: bool,
        join: ReplyJoin,
    ) -> Result<ReplyWait, ReplyErr> {
        if self.targets.len() != outcomes.len() {
            return Err(ReplyErr::OutcomeCount {
                targets: self.targets.len(),
                outcomes: outcomes.len(),
            });
        }
        let legs = self
            .targets
            .into_iter()
            .zip(outcomes)
            .map(|(target, outcome)| Leg::new(target, outcome, self.wait_base))
            .collect();
        Ok(ReplyWait {
            legs,
            steer,
            join,
            caller_identity: self.caller_identity,
            tick: 0,
        })
    }
}

pub struct ReplyWait {
    legs: Vec<Leg>,
    steer: bool,
    join: ReplyJoin,
    caller_identity: Option<(AgentKind, String)>,
    tick: u8,
}

impl ReplyWait {
    pub fn progress(&self) -> ReplyProgress {
        let pending = self
            .legs
            .iter()
            .filter(|leg| leg.done.is_none())
            .collect::<Vec<_>>();
        if let [leg] = pending.as_slice() {
            ReplyProgress::Target {
                label: leg.target.label.clone(),
                parked: matches!(
                    leg.message_status,
                    MessageStatus::Queued | MessageStatus::Claimed
                ),
            }
        } else {
            ReplyProgress::Fanout {
                pending: pending.len(),
                total: self.legs.len(),
            }
        }
    }

    pub fn poll(&mut self, store: &Store) -> Result<ReplyUpdate, ReplyErr> {
        let initial = self.take_unreported();
        if !initial.is_empty() {
            return Ok(self.update(initial));
        }
        if let Some(join) = self.join_result(None) {
            return Ok(ReplyUpdate {
                settled: Vec::new(),
                join: Some(join),
            });
        }

        let messages = store.list_messages()?;
        let mut snapshot = store.snapshot_cached()?;
        let mut newly_settled = Vec::new();
        if self.tick == 0
            && let Some((self_kind, self_name)) = self.caller_identity.as_ref()
        {
            snapshot = snapshot
                .with_agent_context(crate::store::agent_context::read_all(store.runtime_paths()));
            let history = store.list_message_history()?;
            for (index, cycle) in deadlocked_legs(
                &self.legs, &messages, &history, &snapshot, self_kind, self_name,
            ) {
                let leg = &mut self.legs[index];
                let first = cycle.first();
                leg.done = Some(RunStatus::Failed);
                leg.failure = Some(ReplyFailure::Deadlock {
                    first_handle: first.map(|hop| hop.handle.clone()),
                    first_message_id: first.map(|hop| hop.message_id.clone()),
                    chain: render_chain(&cycle),
                });
                newly_settled.push(index);
            }
        }
        for (index, leg) in self.legs.iter_mut().enumerate() {
            if leg.done.is_some() {
                continue;
            }
            if advance_leg(leg, store, &messages, &snapshot, self.steer)? {
                newly_settled.push(index);
            }
        }
        self.tick = (self.tick + 1) % WAIT_GUARD_TICKS;
        for index in &newly_settled {
            self.legs[*index].reported = true;
        }
        Ok(self.update(newly_settled))
    }

    pub fn timeout(&mut self, store: &Store, session_name: &str) -> Result<ReplyUpdate, ReplyErr> {
        let mut newly_settled = self.take_unreported();
        for (index, leg) in self.legs.iter_mut().enumerate() {
            if leg.done.is_some() {
                continue;
            }
            if leg.message_status == MessageStatus::Sent {
                let _ =
                    store.mark_message_timed_out(&leg.message_id, session_name, Some("wait"))?;
            }
            leg.done = Some(RunStatus::TimedOut);
            leg.reported = true;
            newly_settled.push(index);
        }
        let settled = newly_settled
            .into_iter()
            .map(|index| self.legs[index].result())
            .collect();
        Ok(ReplyUpdate {
            settled,
            join: Some(ReplyJoinResult {
                status: RunStatus::TimedOut,
                winner: None,
            }),
        })
    }

    fn take_unreported(&mut self) -> Vec<usize> {
        self.legs
            .iter_mut()
            .enumerate()
            .filter_map(|(index, leg)| {
                (leg.done.is_some() && !std::mem::replace(&mut leg.reported, true)).then_some(index)
            })
            .collect()
    }

    fn update(&self, settled_indices: Vec<usize>) -> ReplyUpdate {
        let winner = (self.join == ReplyJoin::Any)
            .then(|| settled_indices.first().copied())
            .flatten();
        let settled = settled_indices
            .into_iter()
            .map(|index| self.legs[index].result())
            .collect();
        ReplyUpdate {
            settled,
            join: self.join_result(winner),
        }
    }

    fn join_result(&self, winner: Option<usize>) -> Option<ReplyJoinResult> {
        if let Some(index) = winner {
            let leg = &self.legs[index];
            return Some(ReplyJoinResult {
                status: leg.done?,
                winner: Some(leg.message_id.clone()),
            });
        }
        if self.join == ReplyJoin::Any || self.legs.iter().any(|leg| leg.done.is_none()) {
            return None;
        }
        let status = self
            .legs
            .iter()
            .filter_map(|leg| leg.done)
            .find(|status| *status != RunStatus::Completed)
            .unwrap_or(RunStatus::Completed);
        Some(ReplyJoinResult {
            status,
            winner: None,
        })
    }
}

struct ReplyTarget {
    kind: AgentKind,
    agent_id: AgentSessionId,
    agent_name: Option<String>,
    label: String,
    cursor: Option<TranscriptCursor>,
    transcript_path: Option<String>,
}

impl ReplyTarget {
    fn new(agent: &AgentState, label: String, adapter: &AgentDefinition) -> Self {
        let transcript_path = agent.transcript_path.clone();
        Self {
            kind: agent.kind.clone(),
            agent_id: agent.agent_id.clone(),
            agent_name: agent.name.clone(),
            label,
            cursor: Some(anchored_cursor(
                transcript_path.as_deref(),
                Some(&agent.agent_id),
                adapter,
            )),
            transcript_path,
        }
    }

    fn matches(&self, agent: &AgentState) -> bool {
        AgentCardRef::new(&self.kind, &self.agent_id, self.agent_name.as_deref())
            .matches(agent.card_ref())
    }
}

struct Leg {
    target: ReplyTarget,
    message_id: MessageId,
    phase: WaitPhase,
    message_status: MessageStatus,
    wait_base: u64,
    cursor: Option<TranscriptCursor>,
    last_message: Option<String>,
    transcript_path: Option<String>,
    done: Option<RunStatus>,
    failure: Option<ReplyFailure>,
    reported: bool,
}

impl Leg {
    fn new(mut target: ReplyTarget, outcome: &DispatchOutcome, wait_base: u64) -> Self {
        let (message_id, message_status, done, failure) = match outcome {
            DispatchOutcome::Sent { message_id, .. } => {
                (message_id.clone(), MessageStatus::Sent, None, None)
            }
            DispatchOutcome::Queued { message_id, .. }
            | DispatchOutcome::CompactionPending { message_id, .. } => {
                (message_id.clone(), MessageStatus::Queued, None, None)
            }
            DispatchOutcome::SkippedWaiting { message_id, .. } => (
                message_id.clone(),
                MessageStatus::Errored,
                Some(RunStatus::Failed),
                Some(ReplyFailure::WaitingForInput),
            ),
        };
        let cursor = (message_status == MessageStatus::Sent)
            .then(|| target.cursor.take())
            .flatten();
        target.cursor = None;
        let transcript_path = target.transcript_path.clone();
        Self {
            target,
            message_id,
            phase: WaitPhase::Delivery,
            message_status,
            wait_base,
            cursor,
            last_message: None,
            transcript_path,
            done,
            failure,
            reported: false,
        }
    }

    fn result(&self) -> ReplyResult {
        ReplyResult {
            label: self.target.label.clone(),
            message_id: self.message_id.clone(),
            // Results are constructed only from indices marked settled.
            status: self.done.expect("settled reply leg has status"),
            final_message: self.last_message.clone(),
            failure: self.failure.clone(),
            transcript_path: self.transcript_path.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitPhase {
    Delivery,
    Reply { turn_started_at: Option<Timestamp> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CardView {
    status: AgentStatus,
    turn_started_at: Option<Timestamp>,
}

impl From<&AgentState> for CardView {
    fn from(agent: &AgentState) -> Self {
        Self {
            status: agent.status,
            turn_started_at: agent.turn_started_at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Wait(WaitPhase),
    Finish(RunStatus),
    DeliveryFailed(MessageStatus),
    AgentGone,
}

fn step(
    phase: WaitPhase,
    steer: bool,
    message_status: MessageStatus,
    card: Option<CardView>,
) -> Step {
    if phase == WaitPhase::Delivery
        && message_status.is_terminal()
        && message_status != MessageStatus::Delivered
    {
        return Step::DeliveryFailed(message_status);
    }
    let Some(card) = card else {
        return Step::AgentGone;
    };
    match phase {
        WaitPhase::Delivery => {
            let delivered = message_status == MessageStatus::Delivered
                || (steer
                    && message_status == MessageStatus::Sent
                    && card.status == AgentStatus::Running);
            if !delivered {
                return Step::Wait(WaitPhase::Delivery);
            }
            step_reply(None, card)
        }
        WaitPhase::Reply { turn_started_at } => step_reply(turn_started_at, card),
    }
}

fn step_reply(turn_started_at: Option<Timestamp>, card: CardView) -> Step {
    match card.status {
        AgentStatus::Idle | AgentStatus::Success => Step::Finish(RunStatus::Completed),
        AgentStatus::Failed => Step::Finish(RunStatus::Failed),
        AgentStatus::Running
            if turn_started_at.is_some()
                && card.turn_started_at.is_some()
                && turn_started_at != card.turn_started_at =>
        {
            Step::Finish(RunStatus::Completed)
        }
        AgentStatus::Running | AgentStatus::Waiting | AgentStatus::Paused => {
            Step::Wait(WaitPhase::Reply {
                turn_started_at: turn_started_at.or(card.turn_started_at),
            })
        }
    }
}

fn advance_leg(
    leg: &mut Leg,
    store: &Store,
    messages: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    steer: bool,
) -> Result<bool, ReplyErr> {
    if let Some(status) =
        current_message_status(store, messages, &leg.message_id, &mut leg.wait_base)?
    {
        leg.message_status = status;
    }
    let agent = snapshot
        .agents
        .iter()
        .find(|agent| leg.target.matches(agent));
    let adapter = crate::agents::find_definition(leg.target.kind.as_str())
        .ok_or_else(|| ReplyErr::UnknownAgentKind(leg.target.kind.clone()))?;
    if let Some(path) = agent.and_then(|agent| agent.transcript_path.as_deref()) {
        leg.transcript_path = Some(path.to_owned());
    }
    if leg.cursor.is_none()
        && matches!(
            leg.message_status,
            MessageStatus::Sent | MessageStatus::Delivered
        )
    {
        leg.cursor = Some(anchored_cursor(
            agent.and_then(|agent| agent.transcript_path.as_deref()),
            agent.map(|agent| &agent.agent_id),
            adapter,
        ));
    } else if let (Some(cursor), Some(agent)) = (&mut leg.cursor, agent) {
        for message in cursor.messages(
            agent.transcript_path.as_deref(),
            Some(&agent.agent_id),
            adapter,
        ) {
            leg.last_message = Some(message);
        }
    }
    match step(
        leg.phase,
        steer,
        leg.message_status,
        agent.map(CardView::from),
    ) {
        Step::Wait(next) => {
            leg.phase = next;
            Ok(false)
        }
        Step::Finish(status) => {
            leg.done = Some(status);
            Ok(true)
        }
        Step::DeliveryFailed(status) => {
            leg.done = Some(RunStatus::Failed);
            leg.failure = Some(ReplyFailure::DeliveryFailed { status });
            Ok(true)
        }
        Step::AgentGone => {
            leg.done = Some(RunStatus::Failed);
            leg.failure = Some(ReplyFailure::AgentGone);
            Ok(true)
        }
    }
}

fn anchored_cursor(
    path: Option<&str>,
    session_id: Option<&AgentSessionId>,
    adapter: &AgentDefinition,
) -> TranscriptCursor {
    let mut cursor = TranscriptCursor::new(false);
    let _ = cursor.messages(path, session_id, adapter);
    cursor
}

fn current_message_status(
    store: &Store,
    messages: &[MessageRecord],
    message_id: &MessageId,
    wait_base: &mut u64,
) -> Result<Option<MessageStatus>, ReplyErr> {
    if let Some(message) = messages
        .iter()
        .find(|message| message.message_id == *message_id)
    {
        return Ok(Some(message.status));
    }
    latest_terminal_message_status(store, message_id, wait_base)
}

fn latest_terminal_message_status(
    store: &Store,
    message_id: &MessageId,
    base: &mut u64,
) -> Result<Option<MessageStatus>, ReplyErr> {
    let mut latest = None;
    let path = &store.paths().events_log;
    let log_len = match std::fs::metadata(path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(source) => {
            return Err(ReplyErr::Io {
                path: path.clone(),
                source,
            });
        }
    };
    if log_len < *base {
        *base = 0;
    }
    let (events, end) = event_log::read_from_offset(path, *base)?;
    *base = end;
    for event in events {
        let EventKind::Message { payload, .. } = event.kind() else {
            continue;
        };
        if payload.message_id == *message_id && payload.status.is_terminal() {
            latest = Some(payload.status);
        }
    }
    Ok(latest)
}

fn preflight_reply_hooks(
    agent: &AgentState,
    adapter: &AgentDefinition,
) -> Result<(), ReplyPrepareErr> {
    match crate::agents::preflight_hooks(adapter, crate::agents::TurnLifecycleNeed::NotUnsupported)
    {
        Ok(()) => Ok(()),
        Err(crate::agents::HookPreflightErr::TurnLifecycleUnsupported { reason }) => {
            Err(ReplyPrepareErr::TurnLifecycleUnsupported {
                kind: agent.kind.clone(),
                reason,
            })
        }
        Err(crate::agents::HookPreflightErr::HooksMissing) => Err(ReplyPrepareErr::HooksMissing {
            kind: agent.kind.clone(),
        }),
        Err(crate::agents::HookPreflightErr::HooksUntrusted { hooks, fix }) => {
            Err(ReplyPrepareErr::HooksUntrusted {
                kind: agent.kind.clone(),
                hooks,
                fix,
            })
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WaitCycleHop {
    handle: String,
    message_id: MessageId,
}

fn render_chain(cycle: &[WaitCycleHop]) -> Option<String> {
    (cycle.len() > 1).then(|| {
        cycle
            .iter()
            .map(|hop| hop.handle.as_str())
            .chain(std::iter::once("you"))
            .collect::<Vec<_>>()
            .join(" → ")
    })
}

#[derive(Clone, Debug)]
struct WaitEdge {
    sender: usize,
    receiver: usize,
    message_id: MessageId,
}

fn wait_cycle(
    live: &[MessageRecord],
    history: &[MessageRecord],
    agents: &[AgentState],
    self_kind: &AgentKind,
    self_name: &str,
    target: &AgentState,
) -> Option<Vec<WaitCycleHop>> {
    let self_index = agents
        .iter()
        .position(|agent| agent.kind == *self_kind && agent.name.as_deref() == Some(self_name))?;
    let target_index = agents.iter().position(|agent| same_card(agent, target))?;
    let mut edges = Vec::new();
    for record in live.iter().filter(|record| {
        record.reply_wait
            && matches!(
                record.status,
                MessageStatus::Queued | MessageStatus::Claimed | MessageStatus::Sent
            )
    }) {
        let Some(sender) = running_sender_index(record, agents) else {
            continue;
        };
        let Some(receiver) = agents
            .iter()
            .position(|agent| record.same_agent_card(agent))
        else {
            continue;
        };
        edges.push(WaitEdge {
            sender,
            receiver,
            message_id: record.message_id.clone(),
        });
    }
    for (receiver, agent) in agents.iter().enumerate() {
        if agent.status != AgentStatus::Running {
            continue;
        }
        let Some(context) = agent.context.as_ref() else {
            continue;
        };
        for message_id in &context.turn_opened_by {
            let Some(record) = history.iter().find(|record| {
                record.message_id == *message_id
                    && record.reply_wait
                    && record.status == MessageStatus::Delivered
            }) else {
                continue;
            };
            let Some(sender) = running_sender_index(record, agents) else {
                continue;
            };
            edges.push(WaitEdge {
                sender,
                receiver,
                message_id: record.message_id.clone(),
            });
        }
    }
    let peers = agents.iter().collect::<Vec<_>>();
    let mut visited = HashSet::new();
    let mut path = Vec::new();
    walk_edges(
        target_index,
        self_index,
        &edges,
        agents,
        &peers,
        &mut visited,
        &mut path,
    )
    .then_some(path)
}

fn youngest_wait_message(cycle: &[WaitCycleHop], own: &MessageId) -> MessageId {
    cycle
        .iter()
        .map(|hop| &hop.message_id)
        .chain(std::iter::once(own))
        .max_by(|left, right| left.as_str().cmp(right.as_str()))
        .cloned()
        .unwrap_or_else(|| own.clone())
}

fn walk_edges(
    current: usize,
    destination: usize,
    edges: &[WaitEdge],
    agents: &[AgentState],
    peers: &[&AgentState],
    visited: &mut HashSet<usize>,
    path: &mut Vec<WaitCycleHop>,
) -> bool {
    if current == destination {
        return true;
    }
    if !visited.insert(current) {
        return false;
    }
    for edge in edges.iter().filter(|edge| edge.sender == current) {
        path.push(WaitCycleHop {
            handle: crate::harness::target::agent_handle(&agents[current], peers, true),
            message_id: edge.message_id.clone(),
        });
        if walk_edges(
            edge.receiver,
            destination,
            edges,
            agents,
            peers,
            visited,
            path,
        ) {
            return true;
        }
        path.pop();
    }
    false
}

fn running_sender_index(record: &MessageRecord, agents: &[AgentState]) -> Option<usize> {
    let MessageSender::Agent {
        kind,
        name: Some(name),
        ..
    } = &record.sender
    else {
        return None;
    };
    agents.iter().position(|agent| {
        agent.kind == *kind
            && agent.name.as_deref() == Some(name)
            && agent.status == AgentStatus::Running
    })
}

fn same_card(left: &AgentState, right: &AgentState) -> bool {
    left.card_ref().matches(right.card_ref())
}

fn deadlocked_legs(
    legs: &[Leg],
    live: &[MessageRecord],
    history: &[MessageRecord],
    snapshot: &SidebarSnapshot,
    self_kind: &AgentKind,
    self_name: &str,
) -> Vec<(usize, Vec<WaitCycleHop>)> {
    legs.iter()
        .enumerate()
        .filter(|(_, leg)| leg.done.is_none())
        .filter_map(|(index, leg)| {
            let target = snapshot
                .agents
                .iter()
                .find(|agent| leg.target.matches(agent))?;
            let cycle = wait_cycle(
                live,
                history,
                &snapshot.agents,
                self_kind,
                self_name,
                target,
            )?;
            (youngest_wait_message(&cycle, &leg.message_id) == leg.message_id)
                .then_some((index, cycle))
        })
        .collect()
}

#[cfg(test)]
mod tests;
